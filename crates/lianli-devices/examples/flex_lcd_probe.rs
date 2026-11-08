//! Sends a ladder of test frames to a TL or SL-INF Flex LCD and reports whether the
//! panel acknowledges each one. Stop the daemon first, the panel interface is exclusive.
//!
//! cargo run --release -p lianli-devices --example flex_lcd_probe -- --pid a019

use anyhow::{bail, Context, Result};
use clap::Parser;
use image::{Rgb, RgbImage};
use lianli_devices::traits::LcdDevice;
use lianli_devices::winusb::lcd::WinUsbLcdDevice;
use std::time::{Duration, Instant};

const SIZE: u32 = 400;
/// The frame acknowledgement wait is two seconds, so a send this slow had no reply.
const UNACKNOWLEDGED: Duration = Duration::from_millis(1_900);

#[derive(Parser)]
struct Args {
    /// Product ID: a019 for SL-INF Flex, a018 for TL Flex.
    #[arg(long, default_value = "a019")]
    pid: String,
    /// USB serial of the panel; the first panel found is used when omitted.
    #[arg(long)]
    serial: Option<String>,
    /// Seconds each frame stays on screen.
    #[arg(long, default_value_t = 6)]
    hold: u64,
    /// Run only this test number.
    #[arg(long)]
    test: Option<usize>,
    /// Print the tests and their payload sizes without opening a panel.
    #[arg(long)]
    list: bool,
}

enum Encoder {
    Turbo(turbojpeg::Subsamp),
    ImageCrate,
}

#[derive(Clone, Copy)]
enum Pattern {
    Solid,
    Gradient { noise: u8 },
}

struct Case {
    name: &'static str,
    pattern: Pattern,
    encoder: Encoder,
    quality: u8,
    repeat: usize,
}

fn cases() -> Vec<Case> {
    use turbojpeg::Subsamp::{None as Sub444, Sub2x2};
    let case = |name, noise, encoder, quality, repeat| Case {
        name,
        pattern: Pattern::Gradient { noise },
        encoder,
        quality,
        repeat,
    };
    vec![
        Case {
            name: "solid red, turbojpeg 4:2:0 q95",
            pattern: Pattern::Solid,
            encoder: Encoder::Turbo(Sub2x2),
            quality: 95,
            repeat: 1,
        },
        case(
            "gradient, turbojpeg 4:2:0 q95",
            0,
            Encoder::Turbo(Sub2x2),
            95,
            1,
        ),
        case("gradient, image crate q95", 0, Encoder::ImageCrate, 95, 1),
        case(
            "gradient, turbojpeg 4:4:4 q95",
            0,
            Encoder::Turbo(Sub444),
            95,
            1,
        ),
        case(
            "gradient, turbojpeg 4:2:0 q95, sent twice",
            0,
            Encoder::Turbo(Sub2x2),
            95,
            2,
        ),
        case(
            "light noise, turbojpeg 4:2:0 q95",
            12,
            Encoder::Turbo(Sub2x2),
            95,
            1,
        ),
        case(
            "medium noise, turbojpeg 4:2:0 q95",
            40,
            Encoder::Turbo(Sub2x2),
            95,
            1,
        ),
        case(
            "heavy noise, turbojpeg 4:2:0 q95",
            160,
            Encoder::Turbo(Sub2x2),
            95,
            1,
        ),
    ]
}

fn render(pattern: Pattern) -> RgbImage {
    let noise = match pattern {
        Pattern::Solid => return RgbImage::from_pixel(SIZE, SIZE, Rgb([220, 20, 20])),
        Pattern::Gradient { noise } => noise,
    };
    let mut state = 0x2545_f491_u32;
    RgbImage::from_fn(SIZE, SIZE, |x, y| {
        let mut jitter = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if noise == 0 {
                0
            } else {
                (state % (u32::from(noise) * 2 + 1)) as i32 - i32::from(noise)
            }
        };
        let channel = |base: u32, jitter: i32| (base as i32 + jitter).clamp(0, 255) as u8;
        Rgb([
            channel(x * 255 / SIZE, jitter()),
            channel(y * 255 / SIZE, jitter()),
            channel(255 - x * 255 / SIZE, jitter()),
        ])
    })
}

fn encode(case: &Case) -> Result<Vec<u8>> {
    let image = render(case.pattern);
    match case.encoder {
        Encoder::Turbo(subsamp) => {
            let source = turbojpeg::Image {
                pixels: image.as_raw().as_slice(),
                width: SIZE as usize,
                pitch: SIZE as usize * 3,
                height: SIZE as usize,
                format: turbojpeg::PixelFormat::RGB,
            };
            Ok(turbojpeg::compress(source, i32::from(case.quality), subsamp)?.to_vec())
        }
        Encoder::ImageCrate => {
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, case.quality)
                .encode_image(&image)?;
            Ok(jpeg)
        }
    }
}

fn open_panel(pid: u16, serial: Option<&str>) -> Result<WinUsbLcdDevice> {
    let devices = lianli_transport::usb::find_usb_devices(0x1cbe, pid)?;
    for device in devices {
        let found = device
            .device_descriptor()
            .ok()
            .and_then(|desc| {
                device
                    .open()
                    .ok()?
                    .read_serial_number_string_ascii(&desc)
                    .ok()
            })
            .unwrap_or_default();
        println!(
            "Found 1cbe:{pid:04x} at bus {} address {} serial {found}",
            device.bus_number(),
            device.address()
        );
        if serial.is_none_or(|wanted| wanted == found) {
            return WinUsbLcdDevice::open(device, pid).context("opening the panel");
        }
    }
    bail!("no matching 1cbe:{pid:04x} panel found")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info,lianli_devices=debug")
        .init();
    let args = Args::parse();
    if args.list {
        for (number, case) in cases().iter().enumerate() {
            println!(
                "Test {}: {} ({} bytes)",
                number + 1,
                case.name,
                encode(case)?.len()
            );
        }
        return Ok(());
    }
    let pid = u16::from_str_radix(args.pid.trim_start_matches("0x"), 16).context("--pid")?;
    let mut panel = open_panel(pid, args.serial.as_deref())?;
    LcdDevice::initialize(&mut panel).context("initializing the panel")?;

    for (number, case) in cases().iter().enumerate().map(|(i, c)| (i + 1, c)) {
        if args.test.is_some_and(|only| only != number) {
            continue;
        }
        let jpeg = encode(case)?;
        println!(
            "\nTest {number}: {} ({} bytes). Watch the screen.",
            case.name,
            jpeg.len()
        );
        for _ in 0..case.repeat {
            let started = Instant::now();
            panel.send_frame(&jpeg)?;
            let elapsed = started.elapsed();
            println!("  sent in {} ms", elapsed.as_millis());
            if elapsed >= UNACKNOWLEDGED {
                println!(
                    "  The panel did not acknowledge this frame. Stopping; power-cycle the fans \
                     before running another test."
                );
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_secs(args.hold));
    }
    Ok(())
}
