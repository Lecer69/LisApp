//! Bindings are generated from the installed OBS and FFmpeg headers, so they always match them.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn pkg_config(args: &[&str]) -> String {
    let output = Command::new("pkg-config").args(args).output().expect("pkg-config");
    assert!(output.status.success(), "pkg-config {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).expect("pkg-config output").trim().to_string()
}

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=build.rs");
    for library in ["obs", "avcodec", "avutil"] {
        println!("cargo:rustc-link-lib={library}");
    }
    // The OBS test records what the plugin hands to libobs by wrapping those calls
    println!("cargo:rustc-link-arg-tests=-Wl,--export-dynamic");

    // libobs's own flags leave out where its headers are
    let obs_headers = format!("-I{}/obs", pkg_config(&["--variable=includedir", "libobs"]));
    let ffmpeg_headers = pkg_config(&["--cflags-only-I", "libavcodec", "libavutil"]);

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(obs_headers)
        .clang_args(ffmpeg_headers.split_whitespace())
        .clang_arg("-DHAVE_OBSCONFIG_H")
        .default_enum_style(bindgen::EnumVariation::Consts)
        .layout_tests(false)
        .generate_comments(false)
        .allowlist_function("obs_register_source_s")
        .allowlist_function("obs_source_output_video2")
        .allowlist_function("obs_source_set_async_unbuffered")
        .allowlist_function("obs_source_set_async_rotation")
        .allowlist_function("obs_source_showing")
        .allowlist_function("obs_properties_create")
        .allowlist_function("obs_properties_add_text")
        .allowlist_function("video_format_get_parameters_for_format")
        .allowlist_function("blog")
        // What the tests drive OBS with
        .allowlist_function("obs_startup")
        .allowlist_function("obs_shutdown")
        .allowlist_function("obs_open_module")
        .allowlist_function("obs_init_module")
        .allowlist_function("obs_source_get_display_name")
        .allowlist_function("obs_source_create")
        .allowlist_function("obs_source_release")
        .allowlist_function("obs_source_inc_showing")
        .allowlist_function("obs_source_dec_showing")
        .allowlist_function("obs_get_source_output_flags")
        .allowlist_function("avcodec_find_decoder")
        .allowlist_function("avcodec_alloc_context3")
        .allowlist_function("avcodec_free_context")
        .allowlist_function("avcodec_open2")
        .allowlist_function("avcodec_send_packet")
        .allowlist_function("avcodec_receive_frame")
        .allowlist_function("av_packet_alloc")
        .allowlist_function("av_packet_free")
        .allowlist_function("av_packet_unref")
        .allowlist_function("av_new_packet")
        .allowlist_function("av_frame_alloc")
        .allowlist_function("av_frame_free")
        .allowlist_function("av_frame_unref")
        .allowlist_type("obs_source_info")
        .allowlist_type("obs_source_frame2")
        .allowlist_var("OBS_SOURCE_ASYNC_VIDEO")
        .allowlist_var("OBS_SOURCE_DO_NOT_DUPLICATE")
        .allowlist_var("LOG_INFO")
        .allowlist_var("LOG_WARNING")
        .allowlist_var("LIBOBS_API_MAJOR_VER")
        .allowlist_var("LIBOBS_API_MINOR_VER")
        .allowlist_var("LIBOBS_API_PATCH_VER")
        .allowlist_var("MAX_AV_PLANES")
        .allowlist_var("MODULE_SUCCESS")
        .allowlist_var("AV_CODEC_FLAG_LOW_DELAY")
        .allowlist_var("FF_THREAD_SLICE")
        .generate()
        .expect("generate bindings");

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    bindings.write_to_file(out.join("bindings.rs")).expect("write bindings");
}
