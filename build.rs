//! Build script.
//!
//! Its only job is to compile the vendored Game Boy core (Peanut-GB, a C99
//! single-header) into a static lib that the Rust binary links against — and
//! ONLY when the `emu` feature is enabled. With `emu` off (the default, and what
//! CI builds), this early-returns and the build needs neither the vendored C nor
//! the Xtensa GCC cross-compiler.
//!
//! The compiler is `xtensa-esp32s3-elf-gcc`, which `. $HOME/export-esp.sh` puts
//! on PATH (installed by espup under ~/.rustup/toolchains/esp/xtensa-esp-elf/).

fn main() {
    // Re-run if our build inputs change, regardless of feature state.
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var_os("CARGO_FEATURE_EMU").is_none() {
        return; // emulator off → nothing to compile.
    }

    // `emugbc` swaps the DMG-only Peanut-GB core for Walnut-CGB (adds Game Boy
    // Color). It exposes the same emu_* ABI, so only the C core + include path
    // change here; the Rust side differs only in the LCD pixel format.
    let gbc = std::env::var_os("CARGO_FEATURE_EMUGBC").is_some();
    let apu = "vendor/peanut_gb/minigb_apu.c";
    let (wrapper, header, libname) = if gbc {
        ("vendor/walnut/wrapper_cgb.c", "vendor/walnut/walnut_cgb.h", "walnut_cgb")
    } else {
        ("vendor/peanut_gb/wrapper.c", "vendor/peanut_gb/peanut_gb.h", "peanut_gb")
    };
    println!("cargo:rerun-if-changed={wrapper}");
    println!("cargo:rerun-if-changed={apu}");
    println!("cargo:rerun-if-changed={header}");
    println!("cargo:rerun-if-changed=vendor/peanut_gb/minigb_apu.h");

    // If the core hasn't been vendored yet, skip the C compile with a warning
    // rather than failing. `cargo check --features emu` then still type-checks the
    // Rust side (the FFI symbols stay unresolved, which only matters at link time;
    // a full `cargo build` needs the vendored source). Vendor it with:
    //   mkdir -p vendor/peanut_gb && curl -sSL \
    //     https://raw.githubusercontent.com/deltabeard/Peanut-GB/master/peanut_gb.h \
    //     -o vendor/peanut_gb/peanut_gb.h
    if !std::path::Path::new(header).exists() {
        println!(
            "cargo:warning=feature `emu` is on but {header} is missing — skipping the \
             C core compile (cargo check still works; a full build/link needs the \
             vendored core header)."
        );
        return;
    }

    let mut build = cc::Build::new();
    build
        // espup's bare-metal Xtensa GCC (its own newlib); cc would otherwise guess
        // a `xtensa-esp32s3-none-elf-gcc` that does not exist.
        .compiler("xtensa-esp32s3-elf-gcc")
        .archiver("xtensa-esp32s3-elf-ar")
        .file(wrapper)
        .file(apu)
        .include("vendor/peanut_gb"); // minigb_apu.h
    if gbc {
        build.include("vendor/walnut");
    }
    build
        // Match the firmware's 16 kHz I2S so the APU needs no resampling (applies
        // to both wrapper.c and minigb_apu.c).
        .define("AUDIO_SAMPLE_RATE", "16000")
        // int16 native-endian samples (what the I2S DMA wants). Selects
        // audio_sample_t in minigb_apu.h.
        .define("MINIGB_APU_AUDIO_FORMAT_S16SYS", None)
        // Xtensa: literal pools/calls can exceed the short-call range in a big
        // image; -mlongcalls lets the assembler relax them.
        .flag("-mlongcalls")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        // Freestanding: no host libc assumptions. We provide mem* via Rust
        // (compiler-builtins) / a tiny shim if a symbol is missing at link.
        .flag("-ffreestanding")
        .flag("-fno-builtin")
        .opt_level_str("s")
        .define("NDEBUG", None)
        .warnings(false)
        .compile(libname);
}
