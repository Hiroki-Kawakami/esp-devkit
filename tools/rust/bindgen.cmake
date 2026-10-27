# cmake -P step behind devkit_rust_crate(): runs bindgen into OUT_RS and appends
# a signature check for every function declared in an EXPORT_HEADERS header.
# Outputs are only replaced when their content changes, so ninja's restat skips
# the crate when a header edit does not change the bindings.

file(STRINGS "${OPTIONS}" options)
set(clang_args "")
foreach(f ${CLANG_ARGS})
    file(STRINGS "${f}" lines)
    list(APPEND clang_args ${lines})
endforeach()

set(tmp_rs "${OUT_RS}.tmp")
# bindgen gives the wrapper path a .c extension, replacing any other.
set(tmp_c "${STATIC_C}.tmp.c")
file(REMOVE "${tmp_c}")
execute_process(
    COMMAND "${BINDGEN}" "${HEADER}" ${options}
            --wrap-static-fns --wrap-static-fns-path "${tmp_c}"
            --depfile "${DEPFILE}" -o "${tmp_rs}" -- ${clang_args}
    RESULT_VARIABLE result)
if(NOT result EQUAL 0)
    message(FATAL_ERROR "bindgen failed for ${HEADER}")
endif()
# bindgen writes no wrapper source when the headers have no static functions.
if(NOT EXISTS "${tmp_c}")
    file(WRITE "${tmp_c}" "")
endif()

if(EXPORT_OPTIONS)
    file(STRINGS "${EXPORT_OPTIONS}" export_options)
    execute_process(
        COMMAND "${BINDGEN}" "${HEADER}" ${export_options} --formatter=none
                -o "${OUT_RS}.exports" -- ${clang_args}
        RESULT_VARIABLE result)
    if(NOT result EQUAL 0)
        message(FATAL_ERROR "bindgen failed for the exports of ${HEADER}")
    endif()
    file(READ "${OUT_RS}.exports" exports)
    string(REGEX MATCHALL "pub fn [A-Za-z0-9_]+" fns "${exports}")
    set(checks "")
    foreach(fn ${fns})
        string(REPLACE "pub fn " "" fn "${fn}")
        string(APPEND checks
            "const _: () = { let _ = if true { self::${fn} } else { crate::${fn} }; };\n")
    endforeach()
    file(APPEND "${tmp_rs}" "${checks}")
endif()

file(COPY_FILE "${tmp_rs}" "${OUT_RS}" ONLY_IF_DIFFERENT)
file(COPY_FILE "${tmp_c}" "${STATIC_C}" ONLY_IF_DIFFERENT)

file(READ "${DEPFILE}" depfile)
string(FIND "${depfile}" ": " colon)
math(EXPR start "${colon} + 2")
string(SUBSTRING "${depfile}" ${start} -1 inputs)
file(WRITE "${DEPFILE}" "${OUT_RS}: ${inputs}")
