//! SD card stack: SPI1 + embedded-sdmmc.
//!
//! Hardware: WWZMDiB 6-pin microSD module on Daisy SPI1. See daisy/BREAKOUT.md.
//!
//!     Module pin  Daisy pad  STM32 pin   Function
//!     VCC         pad 38     —           +3V3
//!     GND         pad 40     —           GND
//!     MISO        D9         PB4         SPI1_MISO
//!     MOSI        D10        PB5         SPI1_MOSI
//!     SCK         D8         PG11        SPI1_SCK
//!     CS          D7         PG10        software-driven GPIO (idle high)
//!
//! This module currently exposes only the construction path so we can
//! compile-check the integration before the physical breakout is built.
//! Actual mount + read happens during hardware bring-up.
#![allow(dead_code)]

use daisy_embassy::pins::{SeedPin7, SeedPin8, SeedPin9, SeedPin10};
use embassy_stm32::Peri;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::mode::Blocking;
use embassy_stm32::peripherals::SPI1;
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{Config as SpiConfig, Spi};
use embassy_stm32::time::Hertz;
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::{SdCard, TimeSource, Timestamp};

/// Concrete blocking-SPI device wrapping SPI1 + a GPIO CS, ready to pass to
/// `SdCard::new`.
pub type SdSpi<'a> = ExclusiveDevice<Spi<'a, Blocking, Master>, Output<'a>, Delay>;

/// Build the SPI1 bus + CS pin into the SpiDevice that embedded-sdmmc wants,
/// then wrap in an `SdCard`. Does NOT initialise the card — that happens
/// lazily on the first block-device call (e.g. `num_bytes()`), which will
/// block until a card is actually present.
///
/// Peripherals are passed in as already-wrapped `Peri<'_, T>` because that's
/// what `board.pins.dN` and `p.SPI1` return after `new_daisy_board!`.
pub fn build_sd_card<'a>(
    spi1: Peri<'a, SPI1>,
    sck: SeedPin8<'a>,
    mosi: SeedPin10<'a>,
    miso: SeedPin9<'a>,
    cs: SeedPin7<'a>,
) -> SdCard<SdSpi<'a>, Delay> {
    // SD spec requires initialising at ≤400 kHz before stepping up. We set
    // a conservative start frequency here; once the card is online the
    // driver will switch to a higher rate on its own if we reconfigure.
    let mut spi_config = SpiConfig::default();
    spi_config.frequency = Hertz(400_000);

    let spi = Spi::new_blocking(spi1, sck, mosi, miso, spi_config);

    // CS is software-driven, idle high.
    let cs = Output::new(cs, Level::High, Speed::Low);

    // ExclusiveDevice ties bus + CS together for embedded-hal-1.0 SpiDevice.
    // The Delay parameter is used by the trait to space out CS pulses; we
    // hand it an embassy-time Delay impl.
    let spi_device = ExclusiveDevice::new(spi, cs, Delay).unwrap();

    SdCard::new(spi_device, Delay)
}

/// Stub TimeSource. embedded-sdmmc uses this to stamp FAT directory entries
/// on writes; reads don't need it. When we add real wall-clock awareness
/// (probably from a MIDI timecode message or USB SOF), wire it in here.
pub struct ZeroTime;

impl TimeSource for ZeroTime {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}
