fn main() {
    println!("cargo:rerun-if-changed=src/host1x/ffmpeg/ffmpeg_shim.c");
    println!("cargo:rerun-if-changed=src/textures/bcn_shim.cpp");

    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set"),
    );
    let source_dir = manifest_dir
        .parent()
        .expect("video_core must be inside the ruzu source directory");
    let workspace_dir = source_dir
        .parent()
        .expect("the ruzu source directory must be inside the workspace");
    let stb_dir = workspace_dir.join("externals/stb");
    let bc_decoder_dir = workspace_dir.join("externals/bc_decoder");

    println!(
        "cargo:rerun-if-changed={}",
        stb_dir.join("stb_dxt.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        stb_dir.join("stb_dxt.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        bc_decoder_dir.join("bc_decoder.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        bc_decoder_dir.join("bc_decoder.h").display()
    );

    let avcodec = pkg_config::Config::new()
        .probe("libavcodec")
        .expect("libavcodec is required for video_core FFmpeg decoding");
    let avutil = pkg_config::Config::new()
        .probe("libavutil")
        .expect("libavutil is required for video_core FFmpeg decoding");
    let libva = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux" | "freebsd") => pkg_config::Config::new().probe("libva").ok(),
        _ => None,
    };

    let mut build = cc::Build::new();
    build.file("src/host1x/ffmpeg/ffmpeg_shim.c");
    build.warnings(false);
    for path in avcodec
        .include_paths
        .iter()
        .chain(avutil.include_paths.iter())
    {
        build.include(path);
    }
    if let Some(libva) = &libva {
        build.define("LIBVA_FOUND", None);
        for path in &libva.include_paths {
            build.include(path);
        }
    }
    build.compile("ruzu_video_core_ffmpeg_shim");

    let mut bcn_build = cc::Build::new();
    bcn_build.cpp(true);
    // Apple Clang still defaults to C++98 when no language standard is
    // specified.
    bcn_build.std("c++17");
    bcn_build.file("src/textures/bcn_shim.cpp");
    bcn_build.file(stb_dir.join("stb_dxt.cpp"));
    bcn_build.file(bc_decoder_dir.join("bc_decoder.cpp"));
    bcn_build.include(&stb_dir);
    bcn_build.include(&bc_decoder_dir);
    bcn_build.warnings(false);
    bcn_build.compile("ruzu_video_core_bcn_shim");

    compile_vulkan_present_shaders(&manifest_dir);
}

fn compile_vulkan_present_shaders(manifest_dir: &std::path::Path) {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR must be set"));
    let shader_dir = manifest_dir.join("src/host_shaders");
    for include in [
        "ffx_a.h",
        "ffx_fsr1.h",
        "fidelityfx_fsr.frag",
        "opengl_smaa.glsl",
        "opengl_present_scaleforce.frag",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            shader_dir.join(include).display()
        );
    }
    let shaders = [
        ("VULKAN_PRESENT_VERT_SPV", "vulkan_present.vert"),
        ("VULKAN_PRESENT_FRAG_SPV", "vulkan_present.frag"),
        ("FXAA_VERT_SPV", "fxaa.vert"),
        ("FXAA_FRAG_SPV", "fxaa.frag"),
        ("SMAA_EDGE_DETECTION_VERT_SPV", "smaa_edge_detection.vert"),
        ("SMAA_EDGE_DETECTION_FRAG_SPV", "smaa_edge_detection.frag"),
        (
            "SMAA_BLENDING_WEIGHT_CALCULATION_VERT_SPV",
            "smaa_blending_weight_calculation.vert",
        ),
        (
            "SMAA_BLENDING_WEIGHT_CALCULATION_FRAG_SPV",
            "smaa_blending_weight_calculation.frag",
        ),
        (
            "SMAA_NEIGHBORHOOD_BLENDING_VERT_SPV",
            "smaa_neighborhood_blending.vert",
        ),
        (
            "SMAA_NEIGHBORHOOD_BLENDING_FRAG_SPV",
            "smaa_neighborhood_blending.frag",
        ),
        ("FULL_SCREEN_TRIANGLE_VERT_SPV", "full_screen_triangle.vert"),
        ("BLIT_COLOR_FLOAT_FRAG_SPV", "blit_color_float.frag"),
        ("BLIT_COLOR_MSAA_FRAG_SPV", "blit_color_msaa.frag"),
        ("BLIT_DEPTH_MSAA_FRAG_SPV", "blit_depth_msaa.frag"),
        (
            "BLIT_DEPTH_STENCIL_MSAA_FRAG_SPV",
            "blit_depth_stencil_msaa.frag",
        ),
        (
            "VULKAN_BLIT_DEPTH_STENCIL_FRAG_SPV",
            "vulkan_blit_depth_stencil.frag",
        ),
        ("VULKAN_COLOR_CLEAR_VERT_SPV", "vulkan_color_clear.vert"),
        ("VULKAN_COLOR_CLEAR_FRAG_SPV", "vulkan_color_clear.frag"),
        (
            "VULKAN_DEPTHSTENCIL_CLEAR_FRAG_SPV",
            "vulkan_depthstencil_clear.frag",
        ),
        (
            "CONVERT_DEPTH_TO_FLOAT_FRAG_SPV",
            "convert_depth_to_float.frag",
        ),
        (
            "CONVERT_FLOAT_TO_DEPTH_FRAG_SPV",
            "convert_float_to_depth.frag",
        ),
        (
            "CONVERT_ABGR8_TO_D24S8_FRAG_SPV",
            "convert_abgr8_to_d24s8.frag",
        ),
        (
            "CONVERT_ABGR8_TO_D32F_FRAG_SPV",
            "convert_abgr8_to_d32f.frag",
        ),
        (
            "CONVERT_D32F_TO_ABGR8_FRAG_SPV",
            "convert_d32f_to_abgr8.frag",
        ),
        (
            "CONVERT_D24S8_TO_ABGR8_FRAG_SPV",
            "convert_d24s8_to_abgr8.frag",
        ),
        (
            "CONVERT_S8D24_TO_ABGR8_FRAG_SPV",
            "convert_s8d24_to_abgr8.frag",
        ),
        (
            "CONVERT_MSAA_TO_NON_MSAA_FRAG_SPV",
            "convert_msaa_to_non_msaa.frag",
        ),
        (
            "CONVERT_NON_MSAA_TO_MSAA_FRAG_SPV",
            "convert_non_msaa_to_msaa.frag",
        ),
        (
            "CONVERT_MSAA_TO_NON_MSAA_COMP_SPV",
            "convert_msaa_to_non_msaa.comp",
        ),
        (
            "CONVERT_NON_MSAA_TO_MSAA_COMP_SPV",
            "convert_non_msaa_to_msaa.comp",
        ),
        ("ASTC_DECODER_COMP_SPV", "astc_decoder.comp"),
        (
            "BLOCK_LINEAR_UNSWIZZLE_3D_BCN_COMP_SPV",
            "block_linear_unswizzle_3d_bcn.comp",
        ),
        ("VULKAN_QUAD_INDEXED_COMP_SPV", "vulkan_quad_indexed.comp"),
        ("VULKAN_UINT8_COMP_SPV", "vulkan_uint8.comp"),
        (
            "QUERIES_PREFIX_SCAN_SUM_COMP_SPV",
            "queries_prefix_scan_sum.comp",
        ),
        (
            "QUERIES_PREFIX_SCAN_SUM_NOSUBGROUPS_COMP_SPV",
            "queries_prefix_scan_sum_nosubgroups.comp",
        ),
        (
            "RESOLVE_CONDITIONAL_RENDER_COMP_SPV",
            "resolve_conditional_render.comp",
        ),
        ("VULKAN_TURBO_MODE_COMP_SPV", "vulkan_turbo_mode.comp"),
        (
            "VULKAN_FIDELITYFX_FSR_VERT_SPV",
            "vulkan_fidelityfx_fsr.vert",
        ),
        (
            "VULKAN_FIDELITYFX_FSR_EASU_FP32_FRAG_SPV",
            "vulkan_fidelityfx_fsr_easu_fp32.frag",
        ),
        (
            "VULKAN_FIDELITYFX_FSR_EASU_FP16_FRAG_SPV",
            "vulkan_fidelityfx_fsr_easu_fp16.frag",
        ),
        (
            "VULKAN_FIDELITYFX_FSR_RCAS_FP32_FRAG_SPV",
            "vulkan_fidelityfx_fsr_rcas_fp32.frag",
        ),
        (
            "VULKAN_FIDELITYFX_FSR_RCAS_FP16_FRAG_SPV",
            "vulkan_fidelityfx_fsr_rcas_fp16.frag",
        ),
        ("PRESENT_BICUBIC_FRAG_SPV", "present_bicubic.frag"),
        ("PRESENT_GAUSSIAN_FRAG_SPV", "present_gaussian.frag"),
        ("PRESENT_AREA_FRAG_SPV", "present_area.frag"),
        ("PRESENT_BSPLINE_FRAG_SPV", "present_bspline.frag"),
        ("PRESENT_LANCZOS_FRAG_SPV", "present_lanczos.frag"),
        ("PRESENT_MITCHELL_FRAG_SPV", "present_mitchell.frag"),
        ("PRESENT_MMPX_FRAG_SPV", "present_mmpx.frag"),
        ("PRESENT_SPLINE1_FRAG_SPV", "present_spline1.frag"),
        ("PRESENT_ZERO_TANGENT_FRAG_SPV", "present_zero_tangent.frag"),
        ("SGSR1_SHADER_VERT_SPV", "sgsr1_shader.vert"),
        ("SGSR1_SHADER_MOBILE_FRAG_SPV", "sgsr1_shader_mobile.frag"),
        (
            "SGSR1_SHADER_MOBILE_EDGE_DIRECTION_FRAG_SPV",
            "sgsr1_shader_mobile_edge_direction.frag",
        ),
        (
            "VULKAN_PRESENT_SCALEFORCE_FP16_FRAG_SPV",
            "vulkan_present_scaleforce_fp16.frag",
        ),
        (
            "VULKAN_PRESENT_SCALEFORCE_FP32_FRAG_SPV",
            "vulkan_present_scaleforce_fp32.frag",
        ),
    ];

    let glslang = std::env::var("GLSLANGVALIDATOR").unwrap_or_else(|_| "glslangValidator".into());
    let mut generated = String::from("// Generated by video_core/build.rs.\n\n");

    for (name, filename) in shaders {
        let source = shader_dir.join(filename);
        println!("cargo:rerun-if-changed={}", source.display());
        let spv_path = out_dir.join(format!("{filename}.spv"));
        let output = Command::new(&glslang)
            .args(["-V", "--quiet", "--target-env", "spirv1.3", "-o"])
            .arg(&spv_path)
            .arg(format!("-I{}", shader_dir.display()))
            .arg(&source)
            .output()
            .unwrap_or_else(|err| panic!("failed to run {glslang}: {err}"));
        if !output.status.success() {
            panic!(
                "failed to compile {} with {}:\nstdout:\n{}\nstderr:\n{}",
                source.display(),
                glslang,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let bytes = fs::read(&spv_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", spv_path.display()));
        assert!(
            bytes.len() % 4 == 0,
            "SPIR-V output for {filename} is not word-aligned"
        );

        generated.push_str(&format!("pub const {name}: &[u32] = &[\n"));
        for chunk in bytes.chunks_exact(4) {
            let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            generated.push_str(&format!("    0x{word:08x},\n"));
        }
        generated.push_str("];\n\n");
    }

    fs::write(out_dir.join("vulkan_present_spv.rs"), generated)
        .expect("failed to write generated Vulkan present SPIR-V Rust module");
}
