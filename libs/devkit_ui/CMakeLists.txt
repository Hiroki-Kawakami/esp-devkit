# UI framework: framebuffer presentation on bsp_rs.
set(srcs "/dev/null")
set(have_pie false)
if(IDF_TARGET STREQUAL "esp32p4")
    set(srcs "src/zoom_p4.S")
    set(have_pie true)
endif()
idf_component_register(
    SRCS ${srcs}
    REQUIRES bsp_rs devkit devkit_gfx idf
    PRIV_REQUIRES rust_rt)
devkit_rust_out_dir(out_dir)
file(CONFIGURE OUTPUT "${out_dir}/pie.rs" CONTENT "pub(crate) const HAVE_PIE: bool = ${have_pie};\n" @ONLY)
devkit_rust_crate(ROOT "src/lib.rs" TESTS)
