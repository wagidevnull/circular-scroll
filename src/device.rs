//! Finding the touchpad and creating the virtual devices.

use std::{io, os::fd::AsRawFd, path::PathBuf, thread, time::Duration};

use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, KeyCode, RelativeAxisCode, UinputAbsSetup,
    raw_stream::RawDevice, uinput::VirtualDevice,
};

pub const VIRTUAL_SUFFIX: &str = "(circular-scroll)";

pub struct Pad {
    pub path: PathBuf,
    pub dev: RawDevice,
}

impl Pad {
    pub fn name(&self) -> &str {
        self.dev.name().unwrap_or("unknown")
    }

    pub fn absinfo(&self, axis: AbsoluteAxisCode) -> io::Result<AbsInfo> {
        self.dev
            .get_absinfo()?
            .find(|(a, _)| *a == axis)
            .map(|(_, info)| info)
            .ok_or_else(|| io::Error::other(format!("device has no {axis:?} axis")))
    }

    /// Wait up to `timeout_ms` for events to be readable (-1 = forever).
    pub fn wait_readable(&self, timeout_ms: i32) -> io::Result<bool> {
        let mut pfd = libc::pollfd {
            fd: self.dev.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if n >= 0 {
                return Ok(n > 0);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

fn is_touchpad(dev: &RawDevice) -> bool {
    let keys = dev.supported_keys();
    let has_key = |k| keys.is_some_and(|s| s.contains(k));
    let axes = dev.supported_absolute_axes();
    let has_axis = |a| axes.is_some_and(|s| s.contains(a));
    has_key(KeyCode::BTN_TOUCH)
        && has_key(KeyCode::BTN_TOOL_FINGER)
        && !has_key(KeyCode::BTN_TOOL_PEN)
        && has_axis(AbsoluteAxisCode::ABS_X)
        && has_axis(AbsoluteAxisCode::ABS_Y)
        && !dev.name().unwrap_or("").ends_with(VIRTUAL_SUFFIX)
}

/// Every device that looks like a touchpad, in /dev/input/eventN order.
pub fn touchpads() -> Vec<Pad> {
    let mut pads: Vec<Pad> = evdev::raw_stream::enumerate()
        .filter(|(_, dev)| is_touchpad(dev))
        .map(|(path, dev)| Pad { path, dev })
        .collect();
    let num = |p: &Pad| {
        p.path
            .to_string_lossy()
            .rsplit("event")
            .next()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    };
    pads.sort_by_key(num);
    pads
}

/// The touchpad called `name` (or the first one if `name` is empty).
/// With `wait`, keep looking until it shows up.
pub fn find_touchpad(name: &str, wait: bool) -> Result<Pad, String> {
    loop {
        if let Some(pad) = touchpads()
            .into_iter()
            .find(|p| name.is_empty() || p.name() == name)
        {
            return Ok(pad);
        }
        if !wait {
            return Err(match name {
                "" => "no touchpad found (are you root?)".into(),
                _ => format!("no touchpad found named '{name}'"),
            });
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// A virtual device that looks exactly like the real touchpad, so libinput
/// treats it the same way.
pub fn clone_of(pad: &Pad) -> io::Result<VirtualDevice> {
    let name = format!("{} {VIRTUAL_SUFFIX}", pad.name());
    let mut b = VirtualDevice::builder()?
        .name(&name)
        .input_id(pad.dev.input_id())
        .with_properties(pad.dev.properties())?;
    if let Some(keys) = pad.dev.supported_keys() {
        b = b.with_keys(keys)?;
    }
    for (axis, info) in pad.dev.get_absinfo()? {
        b = b.with_absolute_axis(&UinputAbsSetup::new(axis, info))?;
    }
    if let Some(rel) = pad.dev.supported_relative_axes() {
        b = b.with_relative_axes(rel)?;
    }
    if let Some(msc) = pad.dev.misc_properties() {
        b = b.with_msc(msc)?;
    }
    b.build()
}

/// The virtual mouse wheel that ring touches turn into.
pub fn wheel() -> io::Result<VirtualDevice> {
    let keys: AttributeSet<KeyCode> = [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE]
        .into_iter()
        .collect();
    let rel: AttributeSet<RelativeAxisCode> = [
        RelativeAxisCode::REL_X,
        RelativeAxisCode::REL_Y,
        RelativeAxisCode::REL_WHEEL,
        RelativeAxisCode::REL_WHEEL_HI_RES,
    ]
    .into_iter()
    .collect();
    VirtualDevice::builder()?
        .name("circular-scroll wheel")
        .with_keys(&keys)?
        .with_relative_axes(&rel)?
        .build()
}
