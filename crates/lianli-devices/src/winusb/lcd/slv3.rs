use anyhow::Result;
use lianli_shared::screen::ScreenInfo;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::core::{SharedTransport, WinUsbLcdCore};
use super::trait_::{BoxedWinUsbLcd, WinUsbLcd};

/// Universal Screen and Vision use the vendor lcd207 command set. The vendor
/// drives TL and SL-INF Flex with its slv3 command set instead: no StopPlay,
/// SetClock or SetFrameRate during setup, StopClock at 0x29 rather than 0x34,
/// and 0x67 addressing the animation layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandSet {
    Lcd207,
    Flex,
}

impl CommandSet {
    pub(crate) fn for_pid(pid: u16) -> Self {
        if matches!(pid, 0xA018 | 0xA019) {
            Self::Flex
        } else {
            Self::Lcd207
        }
    }

    fn stop_play_supported(self) -> bool {
        self == Self::Lcd207
    }
}

pub struct Slv3WinUsbLcd {
    core: WinUsbLcdCore,
    commands: CommandSet,
}

impl Slv3WinUsbLcd {
    pub(crate) fn new(
        device: rusb::Device<lianli_transport::UsbContext>,
        screen: ScreenInfo,
        name: &str,
        commands: CommandSet,
    ) -> Result<Self> {
        let core = WinUsbLcdCore::open(
            device,
            screen,
            name,
            WRITE_TIMEOUT,
            READ_TIMEOUT,
            commands.stop_play_supported(),
        )?;
        Ok(Self { core, commands })
    }

    pub(crate) fn from_shared(
        transport: SharedTransport,
        screen: ScreenInfo,
        name: String,
        commands: CommandSet,
    ) -> Self {
        Self {
            core: WinUsbLcdCore::from_shared(
                transport,
                screen,
                name,
                WRITE_TIMEOUT,
                READ_TIMEOUT,
                commands.stop_play_supported(),
            ),
            commands,
        }
    }

    fn do_init(&mut self) -> Result<()> {
        self.core.init_logging();
        self.core.reset_failure_state();
        self.core.stop_playback()?;
        self.core.read_firmware();
        match self.commands {
            CommandSet::Lcd207 => {
                self.core.set_frame_rate(SLV3_FRAME_RATE)?;
                self.core.query_h264_block();
                let sync = self.core.builder_mut().sync_clock_header_winusb(2);
                self.core.send_command(sync, "SyncClock");
                let stop_clock = self.core.builder_mut().stop_clock_header_winusb();
                self.core.send_command(stop_clock, "StopClock");
                self.core.clear_layers();
            }
            CommandSet::Flex => {
                self.core.query_h264_block();
                self.core.clear_layers();
                self.core.clear_flex_animation_layer();
            }
        }
        self.core.initialized = true;
        Ok(())
    }
}

const WRITE_TIMEOUT: Duration = Duration::from_millis(2_000);
const READ_TIMEOUT: Duration = Duration::from_millis(2_000);
const SLV3_FRAME_RATE: u8 = 120;

impl WinUsbLcd for Slv3WinUsbLcd {
    fn packet_builder(&mut self) -> &mut crate::crypto::PacketBuilder {
        self.core.builder_mut()
    }
    fn observe_h264_transfer(&mut self, transferred: std::sync::Arc<AtomicBool>) {
        self.core.h264_transferred = Some(transferred);
    }
    fn screen_info(&self) -> &ScreenInfo {
        self.core.screen()
    }
    fn firmware_str(&self) -> Option<&str> {
        self.core.firmware_str()
    }
    fn shared_transport(&self) -> SharedTransport {
        self.core.shared_transport()
    }
    fn stop_playback(&mut self) -> Result<()> {
        self.core.stop_playback()
    }
    fn initialize(&mut self) -> Result<()> {
        if !self.core.initialized {
            self.do_init()?;
        }
        Ok(())
    }
    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        if !self.core.initialized {
            self.do_init()?;
        }
        self.core.send_frame(frame)
    }
    fn send_frame_verified(&mut self, frame: &[u8]) -> Result<()> {
        if !self.core.initialized {
            self.do_init()?;
        }
        self.core.send_frame_verified(frame)
    }
    fn set_brightness_val(&mut self, brightness: u8) -> Result<()> {
        self.core.set_brightness_val(brightness)
    }
    fn switch_to_desktop_mode(&mut self) -> Result<()> {
        self.core.switch_to_desktop_mode()
    }
    fn stream_h264(
        &mut self,
        path: &Path,
        looping: bool,
        stop: &AtomicBool,
        fps: f32,
    ) -> Result<()> {
        if !self.core.initialized {
            self.do_init()?;
        }
        self.core.apply_stream_fps(fps)?;
        self.core
            .stream_h264(path, looping, stop, fps, self.core.screen().play_count, 0)
    }
    fn stream_h264_reader(
        &mut self,
        reader: &mut dyn std::io::Read,
        stop: &AtomicBool,
        fps: f32,
    ) -> Result<()> {
        if !self.core.initialized {
            self.do_init()?;
        }
        self.core.apply_stream_fps(fps)?;
        self.core
            .stream_h264_reader(reader, stop, self.core.screen().play_count, 0)
    }
}

pub(crate) fn boxed(
    device: rusb::Device<lianli_transport::UsbContext>,
    screen: ScreenInfo,
    name: &str,
    commands: CommandSet,
) -> Result<BoxedWinUsbLcd> {
    Ok(Box::new(Slv3WinUsbLcd::new(
        device, screen, name, commands,
    )?))
}
