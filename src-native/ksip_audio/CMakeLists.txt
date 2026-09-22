project(ksip_audio)
if(NOT WIN32)
  return()
endif()
list(APPEND MODULES_DETECTED ${PROJECT_NAME})
set(MODULES_DETECTED ${MODULES_DETECTED} PARENT_SCOPE)
add_library(${PROJECT_NAME} OBJECT ksip_audio.cpp)
target_compile_features(${PROJECT_NAME} PRIVATE cxx_std_20)
target_compile_definitions(${PROJECT_NAME} PRIVATE NOMINMAX WIN32_LEAN_AND_MEAN)
target_compile_options(${PROJECT_NAME} PRIVATE /utf-8)
target_include_directories(${PROJECT_NAME} PRIVATE "${KSIP_NATIVE}/include/ksip")
target_link_libraries(${PROJECT_NAME} PRIVATE
  "${KSIP_NATIVE}/lib/ksip_webrtc_audio.lib"
  "${KSIP_NATIVE}/lib/clang_rt.builtins-x86_64.lib"
  winmm crypt32 iphlpapi secur32 oleaut32)
