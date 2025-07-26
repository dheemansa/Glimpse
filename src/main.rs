//! A layer shell application that creates a full-screen, semi-transparent
//! overlay for selecting a region, built using wayland-client directly.
//! The program exits after a region is selected.
//!
//! To run this example, make sure to add the following dependencies to your Cargo.toml:
//! wayland-client = "0.31.10"
//! wayland-protocols-wlr = { version = "0.3.8", features = ["client"] }
//! tempfile = "3.20,0"
//! memmap2 = "0.9.7"
//! image = { version = "0.25", default-features = false, features = ["png"] }
//! env_logger = "0.11.8"
//! log = "0.4.27"
//!
//! You must also place a `crosshair.png` file in the root of your project.

use std::io::Write;
use std::os::unix::io::{AsRawFd, BorrowedFd};
use memmap2::MmapMut;

use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum
};
use wayland_client::protocol::{
    wl_registry, wl_compositor, wl_shm, wl_shm_pool, wl_surface, wl_buffer,
    wl_seat, wl_pointer, wl_keyboard
};

use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    ZwlrLayerShellV1, Layer as WlrLayer
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, ZwlrLayerSurfaceV1, Anchor as WlrAnchor, KeyboardInteractivity
};

#[derive(Debug, Clone, Copy, PartialEq)]
enum SelectionState {
    Idle,
    Selecting { start: (i32, i32) },
}

struct AppState {
    // Globals
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    seat: Option<wl_seat::WlSeat>,

    // Objects
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    cursor_surface: Option<wl_surface::WlSurface>,
    cursor_hotspot: (i32, i32),

    // State
    running: bool,
    width: u32,
    height: u32,
    selection_state: SelectionState,
    current_pos: (i32, i32),
    prev_pos: (i32, i32),
    prev_selection_state: SelectionState,
    needs_redraw: bool,

    // Buffer reuse for overlay
    canvas_data: Option<Vec<u8>>,
    shm_file: Option<std::fs::File>,
    shm_pool: Option<wl_shm_pool::WlShmPool>,
    buffer: Option<wl_buffer::WlBuffer>,
    mmap: Option<memmap2::MmapMut>,
}

impl AppState {
    fn new() -> Self {
        Self {
            compositor: None,
            shm: None,
            layer_shell: None,
            seat: None,
            surface: None,
            layer_surface: None,
            pointer: None,
            keyboard: None,
            cursor_surface: None,
            cursor_hotspot: (0, 0),
            running: true,
            width: 0,
            height: 0,
            selection_state: SelectionState::Idle,
            current_pos: (0, 0),
            prev_pos: (0, 0),
            prev_selection_state: SelectionState::Idle,
            needs_redraw: true,
            canvas_data: None,
            shm_file: None,
            shm_pool: None,
            buffer: None,
            mmap: None,
        }
    }
}

// --- Dispatch Implementations ---

impl Dispatch<wl_registry::WlRegistry, ()> for AppState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
             _: &Connection,
             qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, version, qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, version, qh, ())),
                "zwlr_layer_shell_v1" => state.layer_shell = Some(registry.bind(name, version, qh, ())),
                "wl_seat" => state.seat = Some(registry.bind(name, version, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for AppState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
             _: &Connection,
             qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for AppState {
    fn event(
        state: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
             _: &Connection,
             qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                state.current_pos = (surface_x as i32, surface_y as i32);
                if let Some(cursor_surface) = &state.cursor_surface {
                    let (hx, hy) = state.cursor_hotspot;
                    pointer.set_cursor(serial, Some(cursor_surface), hx, hy);
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                state.current_pos = (surface_x as i32, surface_y as i32);
                if state.selection_state != SelectionState::Idle && (state.current_pos != state.prev_pos || state.selection_state != state.prev_selection_state) {
                    state.needs_redraw = true;
                }
                state.prev_pos = state.current_pos;
                state.prev_selection_state = state.selection_state;
            }
            wl_pointer::Event::Button { button, state: btn_state, .. } => {
                if button == 0x110 { // Left mouse button
                    match btn_state {
                        WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                            if state.selection_state == SelectionState::Idle {
                                state.selection_state = SelectionState::Selecting { start: state.current_pos };
                                state.needs_redraw = true;
                                state.prev_pos = state.current_pos;
                                state.prev_selection_state = state.selection_state;
                            }
                        }
                        WEnum::Value(wl_pointer::ButtonState::Released) => {
                            if let SelectionState::Selecting { start } = state.selection_state {
                                let (x1, y1) = start;
                                let (x2, y2) = state.current_pos;

                                let x = x1.min(x2);
                                let y = y1.min(y2);
                                let width = (x1 - x2).abs();
                                let height = (y1 - y2).abs();

                                println!("{},{},{}x{}", x, y, width, height);
                                state.running = false;
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for AppState {
    fn event(
        state: &mut Self,
        _keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
             _: &Connection,
             _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key { key, state: key_state, .. } = event {
            // Key 1 is ESC
            if key == 1 && key_state == WEnum::Value(wl_keyboard::KeyState::Pressed) {
                println!("Selection cancelled.");
                state.running = false;
            }
        }
    }
}


impl Dispatch<ZwlrLayerSurfaceV1, ()> for AppState {
    fn event(
        state: &mut Self,
        surf: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
        _: &(),
             _: &Connection,
             qh: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, height } = event {
            state.width = width;
            state.height = height;
            surf.ack_configure(serial);
            draw_frame(state, qh);
        } else if let zwlr_layer_surface_v1::Event::Closed = event {
            state.running = false;
        }
    }
}

// No-op handlers for interfaces we don't need to react to
macro_rules! noop_dispatch {
    ($($iface:ty),*) => {
        $(impl Dispatch<$iface, ()> for AppState {
            fn event(_: &mut Self, _: &$iface, _: <$iface as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        })*
    };
}
noop_dispatch!(
    wl_compositor::WlCompositor, wl_shm::WlShm, wl_shm_pool::WlShmPool,
    wl_surface::WlSurface, wl_buffer::WlBuffer, ZwlrLayerShellV1
);


/// Creates a wl_buffer from raw pixel data.
fn create_buffer_from_data(
    state: &AppState,
    qh: &QueueHandle<AppState>,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<wl_buffer::WlBuffer, Box<dyn std::error::Error>> {
    let shm = state.shm.as_ref().unwrap();
    let stride = width * 4;
    let size = (stride * height) as i32;

    let mut file = tempfile::tempfile()?;
    file.write_all(data)?;
    file.flush()?;

    let fd = unsafe { BorrowedFd::borrow_raw(file.as_raw_fd()) };

    let pool = shm.create_pool(fd, size, qh, ());
    let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();

    Ok(buffer)
}

/// Draws the overlay and the current selection rectangle.
fn draw_frame(state: &mut AppState, qh: &QueueHandle<AppState>) {
    let surface = state.surface.as_ref().unwrap();
    let width = state.width;
    let height = state.height;

    if width == 0 || height == 0 { return; }

    let buffer_size = (width * height * 4) as usize;
    let semi_transparent_black = [0x00, 0x00, 0x00, 0x80]; // BGRA
    let fully_transparent = [0x00, 0x00, 0x00, 0x00];

    // Allocate canvas_data once
    if state.canvas_data.is_none() || state.canvas_data.as_ref().unwrap().len() != buffer_size {
        state.canvas_data = Some(vec![0; buffer_size]);
    }
    let canvas_data = state.canvas_data.as_mut().unwrap();

    // Fill the background
    for chunk in canvas_data.chunks_exact_mut(4) {
        chunk.copy_from_slice(&semi_transparent_black);
    }

    // If selecting, draw the selection rectangle and its border
    if let SelectionState::Selecting { start } = state.selection_state {
        let (x1, y1) = start;
        let (x2, y2) = state.current_pos;

        let min_x = x1.min(x2).max(0) as u32;
        let max_x = x1.max(x2).min(width as i32) as u32;
        let min_y = y1.min(y2).max(0) as u32;
        let max_y = y1.max(y2).min(height as i32) as u32;

        // Draw transparent fill
        for y in min_y..max_y {
            for x in min_x..max_x {
                let offset = ((y * width + x) * 4) as usize;
                if offset + 3 < canvas_data.len() {
                    canvas_data[offset..offset + 4].copy_from_slice(&fully_transparent);
                }
            }
        }

        // Draw white border
        let white = [0xFF, 0xFF, 0xFF, 0xFF]; // BGRA
        // Top border
        if min_y < height {
            for x in min_x..max_x {
                let offset = ((min_y * width + x) * 4) as usize;
                if offset + 3 < canvas_data.len() {
                    canvas_data[offset..offset + 4].copy_from_slice(&white);
                }
            }
        }
        // Bottom border
        if max_y > min_y && max_y - 1 < height {
            for x in min_x..max_x {
                let offset = (((max_y - 1) * width + x) * 4) as usize;
                if offset + 3 < canvas_data.len() {
                    canvas_data[offset..offset + 4].copy_from_slice(&white);
                }
            }
        }
        // Left border
        for y in min_y..max_y {
            if min_x < width {
                let offset = ((y * width + min_x) * 4) as usize;
                if offset + 3 < canvas_data.len() {
                    canvas_data[offset..offset + 4].copy_from_slice(&white);
                }
            }
        }
        // Right border
        if max_x > min_x {
            for y in min_y..max_y {
                if max_x - 1 < width {
                    let offset = ((y * width + (max_x - 1)) * 4) as usize;
                    if offset + 3 < canvas_data.len() {
                        canvas_data[offset..offset + 4].copy_from_slice(&white);
                    }
                }
            }
        }
    }

    // Create shared memory pool, buffer, and mmap once
    if state.shm_file.is_none() || state.shm_pool.is_none() || state.buffer.is_none() || state.mmap.is_none() {
        use std::os::unix::io::AsRawFd;
        let shm = state.shm.as_ref().unwrap();
        let stride = width * 4;
        let size = (stride * height) as i32;

        // Create a temp file for shared memory
        let file = tempfile::tempfile().expect("Failed to create shm file");
        file.set_len(size as u64).expect("Failed to set shm file size");
        let fd = unsafe { BorrowedFd::borrow_raw(file.as_raw_fd()) };

        let pool = shm.create_pool(fd, size, qh, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, wl_shm::Format::Argb8888, qh, ());

        let mmap = unsafe { memmap2::MmapMut::map_mut(&file).expect("Failed to mmap shm file") };

        state.shm_file = Some(file);
        state.shm_pool = Some(pool);
        state.buffer = Some(buffer);
        state.mmap = Some(mmap);
    }

    // Write canvas_data to mmap
    let mmap = state.mmap.as_mut().unwrap();
    mmap[..canvas_data.len()].copy_from_slice(canvas_data);
    mmap.flush().expect("Failed to flush mmap");

    let buffer = state.buffer.as_ref().unwrap();
    surface.attach(Some(buffer), 0, 0);
    surface.damage_buffer(0, 0, width as i32, height as i32);
    surface.commit();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let conn = Connection::connect_to_env()?;
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let display = conn.display();
    display.get_registry(&qh, ());

    let mut state = AppState::new();

    event_queue.roundtrip(&mut state)?;

    let compositor = state.compositor.as_ref().expect("No wl_compositor global");
    let layer_shell = state.layer_shell.as_ref().expect("No zwlr_layer_shell_v1 global");
    state.shm.as_ref().expect("No wl_shm global");

    let surface = compositor.create_surface(&qh, ());
    let layer_surface = layer_shell.get_layer_surface(&surface, None, WlrLayer::Overlay, "rust-layer".into(), &qh, ());
    layer_surface.set_size(0, 0);
    layer_surface.set_anchor(WlrAnchor::Top | WlrAnchor::Bottom | WlrAnchor::Left | WlrAnchor::Right);
    layer_surface.set_exclusive_zone(-1);
    layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();

    state.surface = Some(surface);
    state.layer_surface = Some(layer_surface);
    state.canvas_data = None;
    state.shm_file = None;
    state.shm_pool = None;
    state.buffer = None;
    state.mmap = None;

    let png_bytes = include_bytes!("../assets/crosshair.png");
    let img = image::load_from_memory(png_bytes)?.to_rgba8();
    let (width, height) = img.dimensions();
    let mut rgba_data = img.into_raw();

    for chunk in rgba_data.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    let cursor_surface = compositor.create_surface(&qh, ());
    let cursor_buffer = create_buffer_from_data(&state, &qh, width, height, &rgba_data)?;
    cursor_surface.attach(Some(&cursor_buffer), 0, 0);
    cursor_surface.commit();

    state.cursor_surface = Some(cursor_surface);
    state.cursor_hotspot = ((width / 2) as i32, (height / 2) as i32);

    event_queue.roundtrip(&mut state)?;

    use std::time::{Instant, Duration};
    println!("Click and drag to select a region. Press ESC to cancel.");
    let mut last_frame = Instant::now();
    let frame_interval = Duration::from_millis(33); // ~30 FPS
    while state.running {
        let now = Instant::now();
        if now.duration_since(last_frame) > frame_interval {
            if state.needs_redraw {
                draw_frame(&mut state, &qh);
                state.needs_redraw = false;
            }
            last_frame = now;
        }
        event_queue.blocking_dispatch(&mut state)?;
    }

    println!("Exiting.");
    Ok(())
}
