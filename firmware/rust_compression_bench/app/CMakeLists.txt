# Times devkit_compression against compression_framework on the same streams.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES compression_framework devkit devkit_compression idf rust_rt)

if(NOT DEFINED PYTHON)
    set(PYTHON python3)
endif()

devkit_rust_out_dir(out_dir)
set(assets "${out_dir}/raw.bin" "${out_dir}/zlib.bin" "${out_dir}/gif.bin" "${out_dir}/tiff.bin")
add_custom_command(
    OUTPUT ${assets}
    COMMAND ${PYTHON} "${CMAKE_CURRENT_SOURCE_DIR}/gen_assets.py" "${out_dir}"
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/gen_assets.py"
    VERBATIM)

devkit_rust_crate(ROOT "lib.rs"
    BINDGEN_HEADERS "cmpf_source.h" "cmpf_inflate.h" "cmpf_lzw.h"
    DEPENDS ${assets})
