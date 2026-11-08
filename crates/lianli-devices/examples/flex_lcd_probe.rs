//! Sends test frames and a short H.264 clip to a TL or SL-INF Flex LCD and reports
//! whether the panel acknowledges each one. Stop the daemon first, the panel
//! interface is exclusive. Steps run in order and stop at the first failure.
//!
//! cargo run --release -p lianli-devices --example flex_lcd_probe -- --pid a019

use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use image::{Rgb, RgbImage};
use lianli_devices::traits::LcdDevice;
use lianli_devices::winusb::lcd::WinUsbLcdDevice;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use turbojpeg::Subsamp;

const SIZE: u32 = 400;
/// The frame acknowledgement wait is two seconds, so a send this slow had no reply.
const UNACKNOWLEDGED: Duration = Duration::from_millis(1_900);
const CLIP_FPS: u32 = 30;
const CLIP_SECONDS: u32 = 10;

#[derive(Parser)]
struct Args {
    /// Product ID: a019 for SL-INF Flex, a018 for TL Flex.
    #[arg(long, default_value = "a019")]
    pid: String,
    /// USB serial of the panel; the first panel found is used when omitted.
    #[arg(long)]
    serial: Option<String>,
    /// Seconds each step stays on screen.
    #[arg(long, default_value_t = 6)]
    hold: u64,
    /// Run only this step number.
    #[arg(long)]
    test: Option<usize>,
    /// Print the steps and their JPEG sizes without opening a panel.
    #[arg(long)]
    list: bool,
}

#[derive(Clone, Copy)]
enum Step {
    Jpeg {
        name: &'static str,
        noise: Option<u8>,
        subsamp: Subsamp,
    },
    H264Clip,
}

fn steps() -> [Step; 7] {
    let jpeg = |name, noise, subsamp| Step::Jpeg {
        name,
        noise,
        subsamp,
    };
    [
        jpeg("solid red, 4:4:4", None, Subsamp::None),
        jpeg("smooth gradient, 4:4:4", Some(0), Subsamp::None),
        jpeg("lightly grainy gradient, 4:4:4", Some(12), Subsamp::None),
        jpeg("grainy gradient, 4:4:4", Some(40), Subsamp::None),
        jpeg("very grainy gradient, 4:4:4", Some(160), Subsamp::None),
        Step::H264Clip,
        jpeg("solid red, 4:2:0 (expected to hang)", None, Subsamp::Sub2x2),
    ]
}

fn render(noise: Option<u8>) -> RgbImage {
    let Some(noise) = noise else {
        return RgbImage::from_pixel(SIZE, SIZE, Rgb([220, 20, 20]));
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

fn encode_jpeg(noise: Option<u8>, subsamp: Subsamp) -> Result<Vec<u8>> {
    let image = render(noise);
    let source = turbojpeg::Image {
        pixels: image.as_raw().as_slice(),
        width: SIZE as usize,
        pitch: SIZE as usize * 3,
        height: SIZE as usize,
        format: turbojpeg::PixelFormat::RGB,
    };
    Ok(turbojpeg::compress(source, 95, subsamp)?.to_vec())
}

/// Same libx264 settings the daemon uses for file video.
fn encode_clip(output: &Path) -> Result<()> {
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=size={SIZE}x{SIZE}:rate={CLIP_FPS}"))
        .args(["-t", &CLIP_SECONDS.to_string(), "-r", &CLIP_FPS.to_string()])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-x264opts",
            "bframes=0",
        ])
        .args(["-threads", "4", "-pix_fmt", "yuv420p", "-color_range", "pc"])
        .args(["-an", "-f", "h264"])
        .arg(output)
        .status()
        .context("running ffmpeg")?;
    ensure!(status.success(), "ffmpeg failed to encode the test clip");
    Ok(())
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

/// Returns false when the panel stopped responding.
fn run(panel: &mut WinUsbLcdDevice, number: usize, step: Step, clip: &Path) -> Result<bool> {
    match step {
        Step::Jpeg {
            name,
            noise,
            subsamp,
        } => {
            let jpeg = encode_jpeg(noise, subsamp)?;
            println!(
                "\nStep {number}: {name} JPEG ({} bytes). Watch the screen.",
                jpeg.len()
            );
            let started = Instant::now();
            panel.send_frame(&jpeg)?;
            let elapsed = started.elapsed();
            println!("  sent in {} ms", elapsed.as_millis());
            Ok(elapsed < UNACKNOWLEDGED)
        }
        Step::H264Clip => {
            encode_clip(clip)?;
            println!(
                "\nStep {number}: {CLIP_SECONDS} s H.264 test pattern with a running counter ({} \
                 bytes). Watch the screen.",
                std::fs::metadata(clip)?.len()
            );
            let started = Instant::now();
            let result = panel.stream_h264(clip, false, &AtomicBool::new(false), CLIP_FPS as f32);
            println!("  streamed in {} ms", started.elapsed().as_millis());
            match result {
                Ok(()) => Ok(true),
                Err(error) => {
                    println!("  H.264 stream failed: {error:#}");
                    Ok(false)
                }
            }
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info,lianli_devices=debug")
        .init();
    let args = Args::parse();
    if args.list {
        for (index, step) in steps().into_iter().enumerate() {
            match step {
                Step::Jpeg {
                    name,
                    noise,
                    subsamp,
                } => println!(
                    "Step {}: {name} JPEG ({} bytes)",
                    index + 1,
                    encode_jpeg(noise, subsamp)?.len()
                ),
                Step::H264Clip => println!("Step {}: H.264 test clip", index + 1),
            }
        }
        return Ok(());
    }
    let pid = u16::from_str_radix(args.pid.trim_start_matches("0x"), 16).context("--pid")?;
    let mut panel = open_panel(pid, args.serial.as_deref())?;
    LcdDevice::initialize(&mut panel).context("initializing the panel")?;
    let clip: PathBuf =
        std::env::temp_dir().join(format!("flex-probe-{}.h264", std::process::id()));

    for (index, step) in steps().into_iter().enumerate() {
        let number = index + 1;
        if args.test.is_some_and(|only| only != number) {
            continue;
        }
        let responding = run(&mut panel, number, step, &clip);
        let _ = std::fs::remove_file(&clip);
        if !responding? {
            println!(
                "  The panel stopped responding. Stopping here; power-cycle the fans before \
                 running another step."
            );
            return Ok(());
        }
        let playback = match step {
            Step::H264Clip => u64::from(CLIP_SECONDS),
            Step::Jpeg { .. } => 0,
        };
        std::thread::sleep(Duration::from_secs(args.hold + playback));
    }
    Ok(())
}
