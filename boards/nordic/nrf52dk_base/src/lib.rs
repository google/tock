//! Shared setup for nrf52dk boards.

#![no_std]

#[allow(unused_imports)]
use kernel::{create_capability, debug, debug_gpio, debug_verbose, static_init};

use capsules::virtual_alarm::VirtualMuxAlarm;
use kernel::capabilities;
use kernel::common::dynamic_deferred_call::{DynamicDeferredCall, DynamicDeferredCallClientState};
use kernel::component::Component;
use nrf52::gpio::Pin;
use nrf52::rtc::Rtc;
use nrf52::uicr::Regulator0Output;

pub mod nrf52_components;
use nrf52_components::ble::BLEComponent;

// Constants related to the configuration of the 15.4 network stack
const SRC_MAC: u16 = 0xf00f;
const PAN_ID: u16 = 0xABCD;

/// Pins for SPI for the flash chip MX25R6435F
#[derive(Debug)]
pub struct SpiMX25R6435FPins {
    chip_select: Pin,
    write_protect_pin: Pin,
    hold_pin: Pin,
}

impl SpiMX25R6435FPins {
    pub fn new(chip_select: Pin, write_protect_pin: Pin, hold_pin: Pin) -> Self {
        Self {
            chip_select,
            write_protect_pin,
            hold_pin,
        }
    }
}

/// Pins for the SPI driver
#[derive(Debug)]
pub struct SpiPins {
    mosi: Pin,
    miso: Pin,
    clk: Pin,
}

impl SpiPins {
    pub fn new(mosi: Pin, miso: Pin, clk: Pin) -> Self {
        Self { mosi, miso, clk }
    }
}

/// Pins for the UART
#[derive(Debug)]
pub struct UartPins {
    rts: Option<Pin>,
    txd: Pin,
    cts: Option<Pin>,
    rxd: Pin,
}

impl UartPins {
    pub fn new(rts: Option<Pin>, txd: Pin, cts: Option<Pin>, rxd: Pin) -> Self {
        Self { rts, txd, cts, rxd }
    }
}

pub enum UartChannel<'a> {
    Pins(UartPins),
    Rtt(components::segger_rtt::SeggerRttMemoryRefs<'a>),
}

/// Supported drivers by the platform
pub struct Platform {
    ble_radio: &'static capsules::ble_advertising_driver::BLE<
        'static,
        nrf52::ble_radio::Radio,
        VirtualMuxAlarm<'static, Rtc<'static>>,
    >,
    ieee802154_radio: Option<&'static capsules::ieee802154::RadioDriver<'static>>,
    button: &'static capsules::button::Button<'static, nrf52::gpio::GPIOPin>,
    pconsole: &'static capsules::process_console::ProcessConsole<
        'static,
        components::process_console::Capability,
    >,
    console: &'static capsules::console::Console<'static>,
    gpio: &'static capsules::gpio::GPIO<'static, nrf52::gpio::GPIOPin>,
    led: &'static capsules::led::LED<'static, nrf52::gpio::GPIOPin>,
    rng: &'static capsules::rng::RngDriver<'static>,
    temp: &'static capsules::temperature::TemperatureSensor<'static>,
    ipc: kernel::ipc::IPC,
    analog_comparator: &'static capsules::analog_comparator::AnalogComparator<
        'static,
        nrf52::acomp::Comparator<'static>,
    >,
    alarm: &'static capsules::alarm::AlarmDriver<
        'static,
        capsules::virtual_alarm::VirtualMuxAlarm<'static, nrf52::rtc::Rtc<'static>>,
    >,
    // The nRF52dk does not have the flash chip on it, so we make this optional.
    nonvolatile_storage:
        Option<&'static capsules::nonvolatile_storage_driver::NonvolatileStorage<'static>>,
}

impl kernel::Platform for Platform {
    fn with_driver<F, R>(&self, driver_num: usize, f: F) -> R
    where
        F: FnOnce(Option<&dyn kernel::Driver>) -> R,
    {
        match driver_num {
            capsules::console::DRIVER_NUM => f(Some(self.console)),
            capsules::gpio::DRIVER_NUM => f(Some(self.gpio)),
            capsules::alarm::DRIVER_NUM => f(Some(self.alarm)),
            capsules::led::DRIVER_NUM => f(Some(self.led)),
            capsules::button::DRIVER_NUM => f(Some(self.button)),
            capsules::rng::DRIVER_NUM => f(Some(self.rng)),
            capsules::ble_advertising_driver::DRIVER_NUM => f(Some(self.ble_radio)),
            capsules::ieee802154::DRIVER_NUM => match self.ieee802154_radio {
                Some(radio) => f(Some(radio)),
                None => f(None),
            },
            capsules::temperature::DRIVER_NUM => f(Some(self.temp)),
            capsules::analog_comparator::DRIVER_NUM => f(Some(self.analog_comparator)),
            capsules::nonvolatile_storage_driver::DRIVER_NUM => {
                f(self.nonvolatile_storage.map_or(None, |nv| Some(nv)))
            }
            kernel::ipc::DRIVER_NUM => f(Some(&self.ipc)),
            _ => f(None),
        }
    }
}

/// Generic function for starting an nrf52dk board.
#[inline]
pub unsafe fn setup_board<I: nrf52::interrupt_service::InterruptService>(
    board_kernel: &'static kernel::Kernel,
    button_rst_pin: Pin,
    gpio_port: &'static nrf52::gpio::Port,
    gpio: &'static capsules::gpio::GPIO<'static, nrf52::gpio::GPIOPin>,
    debug_pin1_index: Pin,
    debug_pin2_index: Pin,
    debug_pin3_index: Pin,
    led: &'static capsules::led::LED<'static, nrf52::gpio::GPIOPin>,
    uart_channel: UartChannel<'static>,
    spi_pins: &SpiPins,
    mx25r6435f: &Option<SpiMX25R6435FPins>,
    button: &'static capsules::button::Button<'static, nrf52::gpio::GPIOPin>,
    ieee802154: bool,
    app_memory: &mut [u8],
    process_pointers: &'static mut [Option<&'static dyn kernel::procs::ProcessType>],
    app_fault_response: kernel::procs::FaultResponse,
    reg_vout: Regulator0Output,
    nfc_as_gpios: bool,
    chip: &'static nrf52::chip::NRF52<I>,
) {
    nrf52_components::startup::NrfStartupComponent::new(nfc_as_gpios, button_rst_pin, reg_vout)
        .finalize(());

    // Create capabilities that the board needs to call certain protected kernel
    // functions.
    let process_management_capability =
        create_capability!(capabilities::ProcessManagementCapability);
    let main_loop_capability = create_capability!(capabilities::MainLoopCapability);
    let memory_allocation_capability = create_capability!(capabilities::MemoryAllocationCapability);

    // Configure kernel debug gpios as early as possible
    kernel::debug::assign_gpios(
        Some(&gpio_port[debug_pin1_index]),
        Some(&gpio_port[debug_pin2_index]),
        Some(&gpio_port[debug_pin3_index]),
    );

    let rtc = &nrf52::rtc::RTC;
    rtc.start();
    let mux_alarm = components::alarm::AlarmMuxComponent::new(rtc)
        .finalize(components::alarm_mux_component_helper!(nrf52::rtc::Rtc));
    let alarm = components::alarm::AlarmDriverComponent::new(board_kernel, mux_alarm)
        .finalize(components::alarm_component_helper!(nrf52::rtc::Rtc));

    let channel: &dyn kernel::hil::uart::Uart = match uart_channel {
        UartChannel::Pins(uart_pins) => {
            nrf52::uart::UARTE0.initialize(
                nrf52::pinmux::Pinmux::new(uart_pins.txd as u32),
                nrf52::pinmux::Pinmux::new(uart_pins.rxd as u32),
                uart_pins.cts.map(|x| nrf52::pinmux::Pinmux::new(x as u32)),
                uart_pins.rts.map(|x| nrf52::pinmux::Pinmux::new(x as u32)),
            );
            &nrf52::uart::UARTE0
        }
        UartChannel::Rtt(rtt_memory) => {
            let rtt = components::segger_rtt::SeggerRttComponent::new(mux_alarm, rtt_memory)
                .finalize(components::segger_rtt_component_helper!(nrf52::rtc::Rtc));
            rtt
        }
    };

    let dynamic_deferred_call_clients =
        static_init!([DynamicDeferredCallClientState; 2], Default::default());
    let dynamic_deferred_caller = static_init!(
        DynamicDeferredCall,
        DynamicDeferredCall::new(dynamic_deferred_call_clients)
    );
    DynamicDeferredCall::set_global_instance(dynamic_deferred_caller);

    // Create a shared UART channel for the console and for kernel debug.
    let uart_mux =
        components::console::UartMuxComponent::new(channel, 115200, dynamic_deferred_caller)
            .finalize(());

    let pconsole =
        components::process_console::ProcessConsoleComponent::new(board_kernel, uart_mux)
            .finalize(());

    // Setup the console.
    let console = components::console::ConsoleComponent::new(board_kernel, uart_mux).finalize(());
    // Create the debugger object that handles calls to `debug!()`.
    components::debug_writer::DebugWriterComponent::new(uart_mux).finalize(());

    let ble_radio =
        BLEComponent::new(board_kernel, &nrf52::ble_radio::RADIO, mux_alarm).finalize(());

    let ieee802154_radio = if ieee802154 {
        let (radio, _mux_mac) = components::ieee802154::Ieee802154Component::new(
            board_kernel,
            &nrf52::ieee802154_radio::RADIO,
            &nrf52::aes::AESECB,
            PAN_ID,
            SRC_MAC,
        )
        .finalize(components::ieee802154_component_helper!(
            nrf52::ieee802154_radio::Radio,
            nrf52::aes::AesECB<'static>
        ));
        Some(radio)
    } else {
        None
    };

    let temp =
        components::temperature::TemperatureComponent::new(board_kernel, &nrf52::temperature::TEMP)
            .finalize(());

    let rng = components::rng::RngComponent::new(board_kernel, &nrf52::trng::TRNG).finalize(());

    // SPI
    let mux_spi = components::spi::SpiMuxComponent::new(&nrf52::spi::SPIM0)
        .finalize(components::spi_mux_component_helper!(nrf52::spi::SPIM));

    nrf52::spi::SPIM0.configure(
        nrf52::pinmux::Pinmux::new(spi_pins.mosi as u32),
        nrf52::pinmux::Pinmux::new(spi_pins.miso as u32),
        nrf52::pinmux::Pinmux::new(spi_pins.clk as u32),
    );

    let nonvolatile_storage: Option<
        &'static capsules::nonvolatile_storage_driver::NonvolatileStorage<'static>,
    > = if let Some(driver) = mx25r6435f {
        let mx25r6435f = components::mx25r6435f::Mx25r6435fComponent::new(
            &gpio_port[driver.write_protect_pin],
            &gpio_port[driver.hold_pin],
            &gpio_port[driver.chip_select] as &dyn kernel::hil::gpio::Pin,
            mux_alarm,
            mux_spi,
        )
        .finalize(components::mx25r6435f_component_helper!(
            nrf52::spi::SPIM,
            nrf52::gpio::GPIOPin,
            nrf52::rtc::Rtc
        ));

        let nonvolatile_storage =
            components::nonvolatile_storage::NonvolatileStorageComponent::new(
                board_kernel,
                mx25r6435f,
                0x60000, // Start address for userspace accessible region
                0x20000, // Length of userspace accessible region
                0,       // Start address of kernel region
                0x60000, // Length of kernel region
            )
            .finalize(components::nv_storage_component_helper!(
                capsules::mx25r6435f::MX25R6435F<
                    'static,
                    capsules::virtual_spi::VirtualSpiMasterDevice<'static, nrf52::spi::SPIM>,
                    nrf52::gpio::GPIOPin,
                    VirtualMuxAlarm<'static, nrf52::rtc::Rtc>,
                >
            ));
        Some(nonvolatile_storage)
    } else {
        None
    };

    // Initialize AC using AIN5 (P0.29) as VIN+ and VIN- as AIN0 (P0.02)
    // These are hardcoded pin assignments specified in the driver
    let analog_comparator = components::analog_comparator::AcComponent::new(
        &nrf52::acomp::ACOMP,
        components::acomp_component_helper!(nrf52::acomp::Channel, &nrf52::acomp::CHANNEL_AC0),
    )
    .finalize(components::acomp_component_buf!(nrf52::acomp::Comparator));

    nrf52_components::NrfClockComponent::new().finalize(());

    let platform = Platform {
        button,
        ble_radio,
        ieee802154_radio,
        pconsole,
        console,
        led,
        gpio,
        rng,
        temp,
        alarm,
        analog_comparator,
        nonvolatile_storage,
        ipc: kernel::ipc::IPC::new(board_kernel, &memory_allocation_capability),
    };

    platform.pconsole.start();
    debug!("Initialization complete. Entering main loop\r");
    debug!("{}", &nrf52::ficr::FICR_INSTANCE);

    extern "C" {
        /// Beginning of the ROM region containing app images.
        static _sapps: u8;

        /// End of the ROM region containing app images.
        ///
        /// This symbol is defined in the linker script.
        static _eapps: u8;
    }
    kernel::procs::load_processes(
        board_kernel,
        chip,
        core::slice::from_raw_parts(
            &_sapps as *const u8,
            &_eapps as *const u8 as usize - &_sapps as *const u8 as usize,
        ),
        app_memory,
        process_pointers,
        app_fault_response,
        &process_management_capability,
    )
    .unwrap_or_else(|err| {
        debug!("Error loading processes!");
        debug!("{:?}", err);
    });

    board_kernel.kernel_loop(&platform, chip, Some(&platform.ipc), &main_loop_capability);
}
