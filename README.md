# Glimpse

**Status:** Under Development 🚧

Glimpse is a Wayland layer shell application for selecting a region of the screen with a smooth, semi-transparent overlay. Built in Rust using wayland-client and related libraries.

## Features
- Click and drag to select a region
- ESC to cancel selection
- Custom crosshair cursor
- Efficient buffer reuse and frame rate throttling

## Requirements
- Wayland compositor
- Rust (cargo)
- See `Cargo.toml` for dependencies

## Usage
```
cargo run --release
```

## Notes
- Place your crosshair image at `assets/crosshair.png`
- This project is actively being improved. Expect frequent changes!

## License
See `LICENSE` for details.
