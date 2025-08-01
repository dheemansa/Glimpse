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

    // Double buffering for overlay
    canvas_data: Option<Vec<u8>>,
    shm_files: [Option<std::fs::File>; 2],
    shm_pools: [Option<wl_shm_pool::WlShmPool>; 2],
    buffers: [Option<wl_buffer::WlBuffer>; 2],
    mmaps: [Option<memmap2::MmapMut>; 2],
    active_buffer: usize,
    prev_selection_rect: Option<(u32, u32, u32, u32)>, // (min_x, min_y, max_x, max_y)
    background_cache: Option<Vec<u8>>, // Clean background
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
            shm_files: [None, None],
            shm_pools: [None, None],
            buffers: [None, None],
            mmaps: [None, None],
            active_buffer: 0,
            prev_selection_rect: None,
            background_cache: None,
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
                // Only redraw on pointer enter if you want cursor feedback (optional)
                if let Some(cursor_surface) = &state.cursor_surface {
                    let (hx, hy) = state.cursor_hotspot;
                    pointer.set_cursor(serial, Some(cursor_surface), hx, hy);
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                state.current_pos = (surface_x as i32, surface_y as i32);
                // Only redraw on motion during selection
                if state.selection_state != SelectionState::Idle &&
                   (state.current_pos != state.prev_pos || state.selection_state != state.prev_selection_state) {
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
                                // End selection and exit; no redraw needed
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
            let size_changed = state.width != width || state.height != height;
            state.width = width;
            state.height = height;
            surf.ack_configure(serial);
            if size_changed {
                // Clean up old resources first
                for i in 0..2 {
                    if let Some(pool) = state.shm_pools[i].take() {
                        pool.destroy();
                    }
                    // Files and mmaps will be dropped automatically when replaced
                    state.shm_files[i] = None;
                    state.buffers[i] = None;
                    state.mmaps[i] = None;
                }
                // Allocate canvas_data and double buffers only if size changed and size is valid
                if width > 0 && height > 0 {
                    let buffer_size = (width * height * 4) as usize;
                    state.canvas_data = Some(vec![0; buffer_size]);
                    // Generate background cache
                    let mut bg = vec![0; buffer_size];
                    let semi_transparent_black = [0x00, 0x00, 0x00, 0x80];
                    for chunk in bg.chunks_exact_mut(4) {
                        chunk.copy_from_slice(&semi_transparent_black);
                    }
                    state.background_cache = Some(bg);
                    use std::os::unix::io::AsRawFd;
                    let shm = state.shm.as_ref().unwrap();
                    let stride = width * 4;
                    let size = (stride * height) as i32;
                    for i in 0..2 {
                        let file = tempfile::tempfile().expect("Failed to create shm file");
                        file.set_len(size as u64).expect("Failed to set shm file size");
                        let fd = unsafe { BorrowedFd::borrow_raw(file.as_raw_fd()) };
                        let pool = shm.create_pool(fd, size, qh, ());
                        let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, wl_shm::Format::Argb8888, qh, ());
                        let mmap = unsafe { memmap2::MmapMut::map_mut(&file).expect("Failed to mmap shm file") };
                        state.shm_files[i] = Some(file);
                        state.shm_pools[i] = Some(pool);
                        state.buffers[i] = Some(buffer);
                        state.mmaps[i] = Some(mmap);
                    }
                    state.active_buffer = 0;
                } else {
                    state.canvas_data = None;
                    state.background_cache = None;
                }
            }
            state.needs_redraw = true; // Always redraw after configure
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
    let surface = match state.surface.as_ref() {
        Some(s) => s,
        None => return,
    };
    let width = state.width;
    let height = state.height;

    // Check for valid size and buffer initialization
    if width == 0 || height == 0 || state.buffers[0].is_none() || state.buffers[1].is_none() || state.mmaps[0].is_none() || state.mmaps[1].is_none() || state.canvas_data.is_none() {
        return;
    }

    let semi_transparent_black = [0x00, 0x00, 0x00, 0x80]; // BGRA
    let fully_transparent = [0x00, 0x00, 0x00, 0x00];
    let canvas_data = state.canvas_data.as_mut().unwrap();

    // Track previous and current selection rectangles, union for dirty region
    let mut dirty_min_x = width;
    let mut dirty_min_y = height;
    let mut dirty_max_x = 0;
    let mut dirty_max_y = 0;
    let mut curr_rect = None;

    // Previous selection rectangle
    if let Some((old_min_x, old_min_y, old_max_x, old_max_y)) = state.prev_selection_rect {
        dirty_min_x = dirty_min_x.min(old_min_x);
        dirty_min_y = dirty_min_y.min(old_min_y);
        dirty_max_x = dirty_max_x.max(old_max_x);
        dirty_max_y = dirty_max_y.max(old_max_y);
    }

    // Current selection rectangle
    if let SelectionState::Selecting { start } = state.selection_state {
        let (x1, y1) = start;
        let (x2, y2) = state.current_pos;

        let min_x = x1.min(x2).max(0) as u32;
        let max_x = x1.max(x2).min(width as i32) as u32;
        let min_y = y1.min(y2).max(0) as u32;
        let max_y = y1.max(y2).min(height as i32) as u32;

        let curr_min_x = min_x.saturating_sub(1);
        let curr_min_y = min_y.saturating_sub(1);
        let curr_max_x = (max_x + 1).min(width);
        let curr_max_y = (max_y + 1).min(height);

        dirty_min_x = dirty_min_x.min(curr_min_x);
        dirty_min_y = dirty_min_y.min(curr_min_y);
        dirty_max_x = dirty_max_x.max(curr_max_x);
        dirty_max_y = dirty_max_y.max(curr_max_y);

        curr_rect = Some((curr_min_x, curr_min_y, curr_max_x, curr_max_y));

        // Save current rectangle for next frame
        state.prev_selection_rect = Some((curr_min_x, curr_min_y, curr_max_x, curr_max_y));
    } else {
        state.prev_selection_rect = None;
    }

    // If no selection and no previous, dirty region is whole screen
    if dirty_max_x <= dirty_min_x || dirty_max_y <= dirty_min_y {
        dirty_min_x = 0;
        dirty_min_y = 0;
        dirty_max_x = width;
        dirty_max_y = height;
    }

    // Fill background only in dirty region using background_cache
    if let Some(bg) = &state.background_cache {
        for y in dirty_min_y..dirty_max_y {
            let row_start = ((y * width + dirty_min_x) * 4) as usize;
            let row_size = ((dirty_max_x - dirty_min_x) * 4) as usize;
            let src = &bg[row_start..row_start + row_size];
            let dst = &mut canvas_data[row_start..row_start + row_size];
            dst.copy_from_slice(src);
        }
    }

    // Draw current selection rectangle and border if present
    if let Some((curr_min_x, curr_min_y, curr_max_x, curr_max_y)) = curr_rect {
        let min_x = curr_min_x + 1;
        let max_x = curr_max_x.saturating_sub(1);
        let min_y = curr_min_y + 1;
        let max_y = curr_max_y.saturating_sub(1);

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

    // Write only the dirty rectangle region to the inactive buffer's mmap
    let inactive = (state.active_buffer + 1) % 2;
    let mmap = match state.mmaps[inactive].as_mut() {
        Some(m) => m,
        None => return,
    };
    let stride = width * 4;
    if dirty_max_x > dirty_min_x && dirty_max_y > dirty_min_y {
        for y in dirty_min_y..dirty_max_y {
            let row_start = ((y * width + dirty_min_x) * 4) as usize;
            let row_size = ((dirty_max_x - dirty_min_x) * 4) as usize;
            let src = &canvas_data[row_start..row_start + row_size];
            let dst = &mut mmap[row_start..row_start + row_size];
            dst.copy_from_slice(src);
        }
    }
    mmap.flush().expect("Failed to flush mmap");

    // Swap buffers and display
    state.active_buffer = inactive;
    let buffer = match state.buffers[state.active_buffer].as_ref() {
        Some(b) => b,
        None => return,
    };
    surface.attach(Some(buffer), 0, 0);
    // Only damage the dirty region
    surface.damage_buffer(
        dirty_min_x as i32,
        dirty_min_y as i32,
        (dirty_max_x - dirty_min_x) as i32,
        (dirty_max_y - dirty_min_y) as i32,
    );
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
    state.shm_files = [None, None];
    state.shm_pools = [None, None];
    state.buffers = [None, None];
    state.mmaps = [None, None];
    state.active_buffer = 0;

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

    // Guarantee initial draw
    draw_frame(&mut state, &qh);

    println!("Click and drag to select a region. Press ESC to cancel.");
    while state.running {
        // Block for events, redraw only when needed
        event_queue.blocking_dispatch(&mut state)?;
        if state.needs_redraw {
            draw_frame(&mut state, &qh);
            state.needs_redraw = false;
        }
    }

    println!("Exiting.");
    Ok(())
}
