# Wayland Screen Selector (Glimpse) more Optimization TODO

Performance optimizations ordered by highest to lowest impact.

## 1. SIMD/Unsafe Pixel Manipulation ⚡ **HIGHEST IMPACT**

**Performance Gain**: 4-8x faster for large pixel operations

**Implementation**:
```rust
// Add dependencies to Cargo.toml
[dependencies]
wide = "0.7"  # Safe SIMD wrapper
# OR
bytemuck = "1.14"  # For unsafe transmutes

// Example SIMD background fill
use wide::*;

fn fill_background_simd(buffer: &mut [u8], color: [u8; 4], width: u32, dirty_region: (u32, u32, u32, u32)) {
    let (min_x, min_y, max_x, max_y) = dirty_region;
    let color_u32 = u32::from_le_bytes(color);
    let simd_color = u32x8::splat(color_u32);
    
    for y in min_y..max_y {
        let row_start = ((y * width + min_x) * 4) as usize;
        let row_end = ((y * width + max_x) * 4) as usize;
        let row = &mut buffer[row_start..row_end];
        
        // Process 8 pixels at once with SIMD
        let chunks = row.chunks_exact_mut(32); // 8 pixels * 4 bytes
        let remainder = chunks.remainder();
        
        for chunk in chunks {
            let pixels = bytemuck::cast_mut::<[u8; 32], u32x8>(chunk);
            *pixels = simd_color;
        }
        
        // Handle remaining pixels
        for pixel in remainder.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
    }
}
```

**Changes Needed**:
- Replace pixel-by-pixel loops in `draw_frame()`
- Implement SIMD versions of background fill, rectangle fill, and border drawing
- Add unsafe blocks with proper documentation
- Benchmark different SIMD widths (128-bit, 256-bit, 512-bit)

---

## 2. Eliminate Full-Screen Redraw Fallback ⚡ **CRITICAL FIX**

**Performance Gain**: Eliminates unnecessary full-screen redraws (massive improvement)

**Implementation**:
```rust
// CURRENT PROBLEMATIC CODE TO REMOVE:
if dirty_max_x <= dirty_min_x || dirty_max_y <= dirty_min_y {
    dirty_min_x = 0;
    dirty_min_y = 0;
    dirty_max_x = width;
    dirty_max_y = height;
}

// REPLACE WITH:
if dirty_max_x <= dirty_min_x || dirty_max_y <= dirty_min_y {
    // No dirty region - skip drawing entirely
    return;
}
```

**Changes Needed**:
- Remove fallback in `draw_frame()`
- Ensure initial overlay draw happens via explicit call (already implemented)
- Add logic to handle first-time drawing separately from updates
- Test edge cases where no dirty region exists

---

## 3. Direct mmap Writing Without Canvas Buffer ⚡ **MAJOR OPTIMIZATION**

**Performance Gain**: ~50% memory bandwidth reduction, eliminates buffer copies

**Implementation**:
```rust
// REMOVE: canvas_data field from AppState
// REMOVE: background_cache field (or repurpose as template)

fn draw_frame_direct(state: &mut AppState, qh: &QueueHandle<AppState>) {
    let width = state.width;
    let height = state.height;
    
    // Calculate dirty region first
    let dirty_region = calculate_dirty_region(state, width, height);
    if dirty_region.is_none() { return; }
    let (dirty_min_x, dirty_min_y, dirty_max_x, dirty_max_y) = dirty_region.unwrap();
    
    // Get inactive buffer for double buffering
    let inactive = (state.active_buffer + 1) % 2;
    let mmap = state.mmaps[inactive].as_mut().unwrap();
    
    // Draw directly to mmap
    fill_background_direct(mmap, width, dirty_min_x, dirty_min_y, dirty_max_x, dirty_max_y);
    
    if let SelectionState::Selecting { start } = state.selection_state {
        draw_selection_direct(mmap, width, start, state.current_pos);
    }
    
    // Flush only dirty region
    let start_offset = (dirty_min_y * width + dirty_min_x) * 4;
    let size = (dirty_max_y - dirty_min_y) * width * 4;
    mmap[start_offset as usize..(start_offset + size) as usize].flush().unwrap();
}
```

**Changes Needed**:
- Remove `canvas_data` and `background_cache` from `AppState`
- Rewrite `draw_frame()` to work directly with mmap
- Create helper functions for direct drawing operations
- Handle double buffering correctly with direct writes

---

## 4. Optimized Border Drawing ⚡ **SIGNIFICANT IMPROVEMENT**

**Performance Gain**: 3-5x faster border rendering

**Implementation**:
```rust
fn draw_border_optimized(buffer: &mut [u8], width: u32, rect: (u32, u32, u32, u32), color: [u8; 4]) {
    let (min_x, min_y, max_x, max_y) = rect;
    let stride = width * 4;
    
    // Top and bottom borders - draw entire rows at once
    for &y in &[min_y, max_y - 1] {
        if y < height {
            let row_start = (y * stride + min_x * 4) as usize;
            let row_size = ((max_x - min_x) * 4) as usize;
            let row = &mut buffer[row_start..row_start + row_size];
            
            // Fill row with color using chunks
            for pixel in row.chunks_exact_mut(4) {
                pixel.copy_from_slice(&color);
            }
        }
    }
    
    // Left and right borders - draw columns with stride
    for &x in &[min_x, max_x - 1] {
        if x < width {
            for y in min_y + 1..max_y - 1 {  // Skip corners already drawn
                let offset = (y * stride + x * 4) as usize;
                buffer[offset..offset + 4].copy_from_slice(&color);
            }
        }
    }
}
```

**Changes Needed**:
- Replace four separate border loops with optimized version
- Use horizontal line drawing for top/bottom borders
- Optimize vertical border drawing with proper stride handling
- Consider SIMD for horizontal borders

---

## 5. Event Batching 🔄 **MODERATE IMPROVEMENT**

**Performance Gain**: Reduces redundant redraws during rapid mouse movement

**Implementation**:
```rust
struct AppState {
    // ... existing fields ...
    pending_redraw: bool,
    last_event_time: std::time::Instant,
    event_batch_timeout: std::time::Duration,
}

impl AppState {
    fn schedule_redraw(&mut self) {
        self.pending_redraw = true;
        self.last_event_time = std::time::Instant::now();
    }
    
    fn should_redraw(&self) -> bool {
        self.pending_redraw && 
        (self.last_event_time.elapsed() > self.event_batch_timeout)
    }
}

// In main loop:
while state.running {
    event_queue.blocking_dispatch(&mut state)?;
    
    // Batch events - only redraw after timeout or on significant events
    if state.should_redraw() {
        draw_frame(&mut state, &qh);
        state.pending_redraw = false;
    }
}
```

**Changes Needed**:
- Add batching timer to `AppState`
- Replace immediate `needs_redraw = true` with `schedule_redraw()`
- Implement timeout-based redraw logic
- Handle immediate redraws for button press/release

---

## 6. Background Cache with Row-Based Copying 📈 **GOOD IMPROVEMENT**

**Performance Gain**: 2-3x faster background filling

**Implementation**:
```rust
fn fill_background_rows(
    canvas: &mut [u8], 
    background: &[u8], 
    width: u32, 
    dirty_region: (u32, u32, u32, u32)
) {
    let (min_x, min_y, max_x, max_y) = dirty_region;
    let stride = width * 4;
    
    for y in min_y..max_y {
        let row_start = (y * stride + min_x * 4) as usize;
        let row_size = ((max_x - min_x) * 4) as usize;
        
        let src = &background[row_start..row_start + row_size];
        let dst = &mut canvas[row_start..row_start + row_size];
        
        // Single memcpy operation per row
        dst.copy_from_slice(src);
    }
}
```

**Changes Needed**:
- Replace pixel-by-pixel background copying
- Use `copy_from_slice` for entire rows
- Pre-calculate row boundaries
- Consider SIMD for very wide rows

---

## 7. Targeted mmap Flushing 🎯 **SMALL BUT CONSISTENT**

**Performance Gain**: Reduces system calls, especially for small updates

**Implementation**:
```rust
use memmap2::MmapMut;

fn flush_dirty_region(mmap: &mut MmapMut, width: u32, dirty_region: (u32, u32, u32, u32)) {
    let (min_x, min_y, max_x, max_y) = dirty_region;
    let stride = width * 4;
    
    // Calculate byte range for dirty region
    let start_offset = (min_y * stride + min_x * 4) as usize;
    let end_offset = ((max_y - 1) * stride + max_x * 4) as usize;
    
    // Flush only the dirty region
    if let Ok(range) = mmap.get_mut(start_offset..end_offset) {
        range.flush().expect("Failed to flush dirty region");
    }
}
```

**Changes Needed**:
- Replace `mmap.flush()` with targeted flushing
- Calculate exact byte ranges for dirty regions
- Handle edge cases (region boundaries, empty regions)
- Test if `memmap2` supports partial flushing on target platform

---

## 8. Compile-Time Cursor Processing 🔧 **MINOR OPTIMIZATION**

**Performance Gain**: Eliminates runtime pixel format conversion

**Implementation**:
```rust
// Create build script: build.rs
use image;
use std::env;
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("cursor_data.rs");
    let mut f = File::create(&dest_path).unwrap();
    
    // Process cursor at build time
    let img = image::load_from_memory(include_bytes!("assets/crosshair.png"))
        .unwrap()
        .to_rgba8();
    let (width, height) = img.dimensions();
    let mut rgba_data = img.into_raw();
    
    // Convert RGBA to BGRA at compile time
    for chunk in rgba_data.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }
    
    writeln!(f, "pub const CURSOR_WIDTH: u32 = {};", width).unwrap();
    writeln!(f, "pub const CURSOR_HEIGHT: u32 = {};", height).unwrap();
    writeln!(f, "pub const CURSOR_DATA: &[u8] = &{:?};", rgba_data).unwrap();
}

// In main.rs:
include!(concat!(env!("OUT_DIR"), "/cursor_data.rs"));

// Remove runtime conversion:
let cursor_buffer = create_buffer_from_data(&state, &qh, CURSOR_WIDTH, CURSOR_HEIGHT, CURSOR_DATA)?;
```

**Changes Needed**:
- Create `build.rs` script
- Move cursor processing to build time
- Remove runtime RGBA->BGRA conversion
- Update Cargo.toml with build dependencies

---

## 9. Hardware Acceleration (Optional) 🚀 **POTENTIALLY MASSIVE**

**Performance Gain**: Could be 10-100x for complex operations

**Implementation**:
```rust
// Add to Cargo.toml
[dependencies]
wgpu = "0.18"  # For GPU compute shaders
# OR
opencl3 = "0.9"  # For OpenCL acceleration

// Example GPU-accelerated fill
async fn fill_background_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &mut [u8]
) {
    // Create compute shader for parallel pixel filling
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fill_shader"),
        source: wgpu::ShaderSource::Wgsl(r#"
            @compute @workgroup_size(64)
            fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
                // GPU parallel pixel filling
            }
        "#.into()),
    });
    
    // Execute GPU compute shader
    // ...
}
```

**Changes Needed**:
- Research Wayland + GPU integration possibilities
- Implement GPU buffer sharing if supported
- Create compute shaders for pixel operations
- Add fallback to CPU implementation
- **NOTE**: This is complex and may not be worth the implementation cost

---

## 10. Code Structure Refactoring 🏗️ **MAINTAINABILITY**

**Performance Gain**: Indirect - enables better optimizations

**Implementation**:
```rust
// src/drawing.rs
pub mod drawing {
    pub fn calculate_dirty_region(state: &AppState) -> Option<(u32, u32, u32, u32)> { ... }
    pub fn fill_background(buffer: &mut [u8], region: (u32, u32, u32, u32)) { ... }
    pub fn draw_selection_border(buffer: &mut [u8], selection: Selection) { ... }
}

// src/events.rs
pub mod events {
    pub fn handle_pointer_event(state: &mut AppState, event: PointerEvent) { ... }
    pub fn handle_keyboard_event(state: &mut AppState, event: KeyboardEvent) { ... }
}

// src/buffers.rs
pub mod buffers {
    pub fn create_double_buffers(state: &mut AppState) -> Result<(), Error> { ... }
    pub fn swap_buffers(state: &mut AppState) { ... }
}
```

**Changes Needed**:
- Split `draw_frame()` into focused functions
- Extract event handling into separate module
- Create buffer management module
- Add proper error handling and documentation

---

## 11. Cursor Feedback Optimization 🖱️ **MINOR IMPROVEMENT**

**Performance Gain**: Reduces unnecessary cursor updates

**Implementation**:
```rust
struct AppState {
    // ... existing fields ...
    cursor_needs_update: bool,
    current_cursor_state: CursorState,
}

#[derive(PartialEq)]
enum CursorState {
    Default,
    Selecting,
}

impl AppState {
    fn update_cursor_if_needed(&mut self, pointer: &wl_pointer::WlPointer, serial: u32) {
        let new_state = match self.selection_state {
            SelectionState::Idle => CursorState::Default,
            SelectionState::Selecting { .. } => CursorState::Selecting,
        };
        
        if self.current_cursor_state != new_state || self.cursor_needs_update {
            if let Some(cursor_surface) = &self.cursor_surface {
                let (hx, hy) = self.cursor_hotspot;
                pointer.set_cursor(serial, Some(cursor_surface), hx, hy);
            }
            self.current_cursor_state = new_state;
            self.cursor_needs_update = false;
        }
    }
}
```

**Changes Needed**:
- Track cursor state changes
- Only update cursor when state actually changes
- Remove redundant cursor updates

---

## 12. Resource Management 🧹 **STABILITY**

**Performance Gain**: Prevents memory leaks, improves long-term stability

**Implementation**:
```rust
impl Drop for AppState {
    fn drop(&mut self) {
        // Clean up Wayland objects
        for i in 0..2 {
            if let Some(buffer) = self.buffers[i].take() {
                buffer.destroy();
            }
            if let Some(pool) = self.shm_pools[i].take() {
                pool.destroy();
            }
        }
        
        if let Some(cursor_surface) = self.cursor_surface.take() {
            cursor_surface.destroy();
        }
        
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
    }
}

// Use RAII wrapper for temporary buffers
struct WaylandBuffer {
    buffer: wl_buffer::WlBuffer,
    pool: wl_shm_pool::WlShmPool,
}

impl Drop for WaylandBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
    }
}
```

**Changes Needed**:
- Implement `Drop` trait for `AppState`
- Create RAII wrappers for Wayland resources
- Ensure cleanup on error conditions
- Add resource leak detection in debug builds

---

## 13. Profiling and Benchmarking 📊 **MEASUREMENT**

**Performance Gain**: Enables data-driven optimization decisions

**Implementation**:
```rust
// Add to Cargo.toml
[dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

// benches/drawing_bench.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_background_fill(c: &mut Criterion) {
    let mut buffer = vec![0u8; 1920 * 1080 * 4];
    let color = [0x00, 0x00, 0x00, 0x80];
    
    c.bench_function("background_fill", |b| {
        b.iter(|| {
            fill_background_optimized(black_box(&mut buffer), black_box(color));
        })
    });
}

// Add flamegraph support
// cargo install flamegraph
// sudo cargo flamegraph --bin screen_selector
```

**Changes Needed**:
- Add criterion benchmarks for critical functions
- Set up flamegraph profiling
- Create performance regression tests
- Add memory usage profiling

---

## Implementation Priority

**Phase 1 (Critical)**: Items 1-4 - These provide the biggest performance gains
**Phase 2 (Important)**: Items 5-8 - Good improvements with reasonable effort
**Phase 3 (Optional)**: Items 9-13 - Nice to have, maintainability, or experimental

Start with eliminating the full-screen redraw fallback (#2) as it's the easiest high-impact change, then move to SIMD optimization (#1) for the biggest overall performance improvement.
