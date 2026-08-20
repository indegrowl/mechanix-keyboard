use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::time::Instant;

use wayland::WlKeyboardKeyState;
use xkbcommon::xkb::ffi::XKB_KEYMAP_FORMAT_TEXT_V1;
use xkbcommon::xkb::{Context, Keymap};

use crate::MechanixKeyboardState;

pub struct VirtualKeyboardState {
    pub start_time: Instant,
}

/// zwp_virtual_keyboard_v1
///
/// Minimal client: once the registry roundtrip has bound the seat and the
/// virtual-keyboard manager, create a virtual keyboard and hand the compositor
/// a standard keymap.
pub fn module<S>() -> impl app::RegisteredModule<MechanixKeyboardState, S> {
    app::Module::new().on(on_start).on(on_pre_poll)
}

pub fn on_start(s: &mut MechanixKeyboardState, _: &app::Start) {
    s.virtual_keyboard_state = Some(VirtualKeyboardState {
        start_time: Instant::now(),
    });
}

/// The seat/manager are only known after the registry roundtrip, so create the
/// virtual keyboard lazily on the first poll where both are available.
fn on_pre_poll(s: &mut MechanixKeyboardState, _: &app::PrePoll) {
    if s.globals.virtual_keyboard.is_some() {
        send_test_key(s);
        return;
    }
    let (Some(seat), Some(manager)) = (
        s.globals.seat.clone(),
        s.globals.virtual_keyboard_manager.clone(),
    ) else {
        tracing::warn!("No vkbd manager found!");
        return;
    };

    let vkbd = manager.create_virtual_keyboard(&seat);

    // Compile a standard keymap from the default rules and serialise it.
    let ctx = Context::new(0);
    let keymap = Keymap::new_from_names(&ctx, "", "", "us", "", None, 0)
        .expect("failed to compile default keymap");
    let text = keymap.get_as_string(XKB_KEYMAP_FORMAT_TEXT_V1);
    let size = text.len() as u32;

    // Back the keymap with a memfd and pass it to the compositor.
    let name = c"mechanix-keyboard-keymap";
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create failed");
    unsafe {
        libc::ftruncate(fd, size as libc::off_t);
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            size as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        assert!(ptr != libc::MAP_FAILED, "mmap failed");
        std::ptr::copy_nonoverlapping(text.as_ptr(), ptr as *mut u8, size as usize);
        libc::munmap(ptr, size as usize);
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };

    vkbd.keymap(XKB_KEYMAP_FORMAT_TEXT_V1, owned.as_fd(), size);

    s.globals.virtual_keyboard = Some(vkbd);
    tracing::info!(size, "sent keymap to virtual keyboard");
}

fn send_test_key(s: &mut MechanixKeyboardState) {
    if s.interactivity
        .pointer
        .pressed(interactivity::pointer::MouseButton::Left)
    {
        if let Some(vkbd) = s.globals.virtual_keyboard.clone() {
            let key = 65;
            let state = WlKeyboardKeyState::Repeated;
            vkbd.key(
                (Instant::now() - s.virtual_keyboard_state.as_ref().unwrap().start_time).as_millis()
                    as u32,
                key,
                state.into(),
            );
            tracing::info!("Sending key...");
        }
    } else if s
        .interactivity
        .pointer
        .just_released(interactivity::pointer::MouseButton::Left)
    {
        if let Some(vkbd) = s.globals.virtual_keyboard.clone() {
            let key = 65;
            let state = WlKeyboardKeyState::Released;
            vkbd.key(
                (Instant::now() - s.virtual_keyboard_state.as_ref().unwrap().start_time).as_millis()
                    as u32,
                key,
                state.into(),
            );
            tracing::info!("Releasing key...");
        }
    }
}
