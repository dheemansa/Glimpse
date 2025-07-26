# Glimpse Features TODO

## Proposed File Structure

src/<br>
├── main.rs              // Entry point, connection setup, main event loop<br>
├── app.rs               // AppState struct and high-level application logic<br>
├── wayland/<br>
│   ├── mod.rs           // Re-exports and common Wayland utilities<br>
│   ├── dispatch.rs      // All Dispatch trait implementations<br>
│   └── globals.rs       // Global binding and registry handling<br>
├── graphics/<br>
│   ├── mod.rs           // Re-exports for graphics components<br>
│   ├── buffer.rs        // Buffer creation, mmap management, double buffering<br>
│   ├── drawing.rs       // Pixel manipulation, dirty region tracking<br>
│   └── cursor.rs        // Cursor loading and management<br>
├── selection.rs         // Selection state machine and geometry calculations<br>
└── config.rs            // Constants, configuration (optional)<br>

## Refactoring & Structure
- [ ] Split code into modules (see proposed src/ structure)
- [ ] Move AppState and SelectionState to app.rs
- [ ] Move Wayland dispatch logic to wayland/dispatch.rs
- [ ] Move buffer and mmap logic to graphics/buffer.rs
- [ ] Move drawing routines to graphics/drawing.rs
- [ ] Move cursor logic to graphics/cursor.rs
- [ ] Move selection state machine to selection.rs
- [ ] Add config.rs for constants

## Performance Optimizations
- [ ] SIMD/unsafe pixel manipulation for drawing
- [ ] Event batching for pointer/keyboard events
- [ ] Targeted mmap flushing (dirty region only)
- [ ] Hardware acceleration (optional)
- [ ] Profiling and benchmarking

## Features & UX
- [ ] Customizable overlay color/transparency
- [ ] Configurable border thickness/color
- [ ] Keyboard shortcuts for cancel/confirm
- [ ] Multi-monitor support
- [ ] Help/about dialog

## Slurp Compatibility & Integration
- [ ] Support major slurp CLI flags:
    - [ ] `-r` (read regions from stdin)
    - [ ] `-b <color>` (background color)
    - [ ] `-f <format>` (output format, e.g., `%l`)
- [ ] Accept region/window specifications from stdin (e.g., `x,y widthxheight [window_id]`)
- [ ] Output selected region/window in requested format to stdout
- [ ] Interactive selection from provided regions/windows
- [ ] Highlight hovered/selected region in overlay
- [ ] Proper exit codes (0 for success, non-zero for cancel/error)
- [ ] Non-blocking/scriptable behavior for use in pipelines
- [ ] Optional: Display window titles/icons/metadata if available

## Testing & Maintenance
- [ ] Add unit tests for selection logic
- [ ] Add integration tests for event handling
- [ ] Document all modules and public APIs
- [ ] Update README with usage and features
