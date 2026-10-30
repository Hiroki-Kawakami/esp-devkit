# Times devkit_image against image_framework on the same pictures.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES devkit devkit_image idf image_framework rust_rt)

if(NOT DEFINED PYTHON)
    set(PYTHON python3)
endif()

devkit_rust_out_dir(out_dir)
set(assets "${out_dir}/png.bin" "${out_dir}/progressive.bin" "${out_dir}/webp_lossy.bin"
           "${out_dir}/webp_lossless.bin")
add_custom_command(
    OUTPUT ${assets}
    COMMAND ${PYTHON} "${CMAKE_CURRENT_SOURCE_DIR}/gen_assets.py" "${out_dir}" 960 540
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/gen_assets.py"
    VERBATIM)

devkit_rust_crate(ROOT "lib.rs"
    BINDGEN_HEADERS "imgf_types.h" "imgf_stream.h" "imgf_decoder.h" "imgf_encoder.h"
                    "imgf_jpegd.h" "imgf_jpege.h" "imgf_pngd.h" "imgf_webpd.h" "imgf_resize.h"
                    "imgf_alloc.h"
    DEPENDS ${assets})
