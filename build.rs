//! Build script.
//!
//! Compiles the vendored C cores the firmware links against, each gated behind its
//! own Cargo feature so the default build (and CI) need neither the vendored C nor
//! the Xtensa GCC cross-compiler:
//!   * `emu`    → the Game Boy core (Peanut-GB / Walnut-CGB, a C99 single-header)
//!   * `player` → the MP3 decoder (minimp3, a single-header, public-domain CC0 lib)
//! With both off (the default), this is a no-op.
//!
//! The compiler is `xtensa-esp32s3-elf-gcc`, which `. $HOME/export-esp.sh` puts
//! on PATH (installed by espup under ~/.rustup/toolchains/esp/xtensa-esp-elf/).

fn main() {
    // Re-run if our build inputs change, regardless of feature state.
    println!("cargo:rerun-if-changed=build.rs");

    build_emu();
    build_player();
}

/// Common Xtensa cross-compile flags shared by every vendored C core.
fn xtensa_build() -> cc::Build {
    let mut build = cc::Build::new();
    build
        // espup's bare-metal Xtensa GCC (its own newlib); cc would otherwise guess
        // a `xtensa-esp32s3-none-elf-gcc` that does not exist.
        .compiler("xtensa-esp32s3-elf-gcc")
        .archiver("xtensa-esp32s3-elf-ar")
        // Xtensa: literal pools/calls can exceed the short-call range in a big
        // image; -mlongcalls lets the assembler relax them.
        .flag("-mlongcalls")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        // Freestanding: no host libc assumptions. We provide mem* via Rust
        // (compiler-builtins) / a tiny shim if a symbol is missing at link.
        .flag("-ffreestanding")
        .flag("-fno-builtin")
        .define("NDEBUG", None);
    build.warnings(false);
    build
}

/// Compile the MP3 decoder (minimp3) — only with `--features player`.
fn build_player() {
    if std::env::var_os("CARGO_FEATURE_PLAYER").is_none() {
        return; // audio MP3 decode off → nothing to compile (WAV is pure Rust).
    }
    let wrapper = "vendor/minimp3/wrapper.c";
    let header = "vendor/minimp3/minimp3.h";
    println!("cargo:rerun-if-changed={wrapper}");
    println!("cargo:rerun-if-changed={header}");

    // Like the emu core: skip the C compile with a warning if minimp3 hasn't been
    // vendored yet, so `cargo check --features player` still type-checks the Rust
    // (the FFI symbols stay unresolved — only a full build/link needs the header).
    // Vendor it with:
    //   mkdir -p vendor/minimp3 && curl -sSL \
    //     https://raw.githubusercontent.com/lieff/minimp3/master/minimp3.h \
    //     -o vendor/minimp3/minimp3.h
    // REQUIRED PATCH after re-vendoring: in `mp3dec_decode_frame`, change the local
    // `mp3dec_scratch_t scratch;` to `static mp3dec_scratch_t scratch;` — that scratch
    // is ~16 KB and as a stack local it overflows the bare-metal task stack on the
    // ESP32-S3 (esp-hal stack-guard panic). Decode is single-threaded here, so a
    // function-static is safe. (See the "ECHOPUTER PATCH" comment in the header.)
    if !std::path::Path::new(header).exists() {
        println!(
            "cargo:warning=feature `player` is on but {header} is missing — skipping \
             the MP3 decoder compile (cargo check still works; a full build/link needs \
             the vendored minimp3 header)."
        );
        return;
    }

    xtensa_build()
        .file(wrapper)
        .include("vendor/minimp3")
        // The MDCT/synthesis filter is on the audio hot path, so favour speed over
        // size here (the GB core uses "s"; flash is 8 MB, size is not the concern).
        .opt_level_str("3")
        .compile("minimp3");
}

/// Compile the Game Boy core (Peanut-GB, or Walnut-CGB with `emugbc`) — only with
/// `--features emu`.
fn build_emu() {
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

    let mut build = xtensa_build();
    build.file(wrapper).file(apu).include("vendor/peanut_gb"); // minigb_apu.h
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
        .opt_level_str("s")
        .compile(libname);
}
