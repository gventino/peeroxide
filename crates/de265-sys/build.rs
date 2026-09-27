//! Builds the vendored libde265 (see `vendor/VENDORED.md`) as a static library, without CMake.

use std::path::PathBuf;

/// `libde265_sources` in libde265's `CMakeLists.txt`.
const SOURCES: &[&str] = &[
    "alloc_pool.cc",
    "bitstream.cc",
    "cabac.cc",
    "contextmodel.cc",
    "de265.cc",
    "deblock.cc",
    "decctx.cc",
    "dpb.cc",
    "fallback-dct.cc",
    "fallback-deblk.cc",
    "fallback-intrapred.cc",
    "fallback-motion.cc",
    "fallback.cc",
    "image-io.cc",
    "image.cc",
    "intrapred.cc",
    "md5.cc",
    "motion.cc",
    "nal-parser.cc",
    "nal.cc",
    "pps.cc",
    "quality.cc",
    "refpic.cc",
    "sao.cc",
    "scan.cc",
    "sei.cc",
    "slice.cc",
    "sps.cc",
    "threads.cc",
    "transform.cc",
    "util.cc",
    "visualize.cc",
    "vps.cc",
    "vui.cc",
];

/// SSE4.1 kernels, compiled with SSE4.1 enabled; `x86/sse.cc` picks them at run time.
const SSE_SOURCES: &[&str] = &[
    "x86/sse-motion.cc",
    "x86/sse-dct.cc",
    "x86/sse-intrapred.cc",
    "x86/sse-deblk.cc",
];

fn main() {
    let vendor = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("vendor");
    let lib = vendor.join("libde265");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").unwrap() == "msvc";
    let x86_64 = arch == "x86_64";

    // What CMake would write from cmake/config.h.in.
    let mut config = String::from("/* config.h, written by build.rs */\n");
    if x86_64 {
        config.push_str("#define HAVE_SSE4_1 1\n");
    }
    if os == "linux" {
        config.push_str("#define HAVE_MALLOC_H 1\n");
    }
    if os != "windows" {
        config.push_str("#define HAVE_POSIX_MEMALIGN 1\n");
    }
    std::fs::write(out.join("config.h"), config).expect("writing config.h");

    let base = || {
        let mut b = cc::Build::new();
        b.cpp(true)
            .std("c++17")
            .include(&vendor)
            .include(&lib)
            .include(&out)
            .define("HAVE_CONFIG_H", None)
            .define("LIBDE265_STATIC_BUILD", None)
            // No logging: a broken stream from the network must not print to the console.
            .warnings(false);
        b
    };

    // One archive, so the link order of its parts never matters. The SSE4.1 kernels need their
    // own flags, so they're compiled separately and added as objects.
    let mut main = base();
    main.files(SOURCES.iter().map(|f| lib.join(f)));
    if x86_64 {
        main.file(lib.join("x86/sse.cc"));
        let mut sse = base();
        sse.files(SSE_SOURCES.iter().map(|f| lib.join(f)));
        if !msvc {
            sse.flag("-msse4.1");
        }
        main.objects(sse.compile_intermediates());
    }
    if os == "windows" {
        let objects = cc::Build::new()
            .file(vendor.join("extra/win32cond.c"))
            .include(vendor.join("extra"))
            .warnings(false)
            .compile_intermediates();
        main.objects(objects);
    }
    main.compile("de265");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor");
}
