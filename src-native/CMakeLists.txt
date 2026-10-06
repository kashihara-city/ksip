# KSIP's own native code, built apart from baresip: the baresip modules (ksip,
# the engine's SIP side; ksip_ctrl, the app's control connection; ksip_audio,
# the audio devices through WebRTC; ksip_audio_filter, the recording and the
# in-band DTMF) and what the app calls directly (app/), all C++. Compiled
# with the clang-cl the pinned WebRTC brings along
# (native.ps1 passes it), so that all of KSIP's code goes through one
# compiler, whose warnings and checks the rest can build on; baresip and the
# libraries are their own CMake projects, compiled with MSVC as released.
#
# The modules go into one archive, ksip_modules.lib. baresip's build sees
# each module as a stub with no sources (patch-baresip.py writes them), so
# that its static module table names them, and the app links the archive
# along with baresip (src-tauri/build.rs).
cmake_minimum_required(VERSION 3.21)
# C stays enabled for libre's re-config.cmake, whose checks (headers, symbols,
# types) run the C compiler; the sources are all C++.
project(ksip_native C CXX)

set(KSIP_NATIVE "" CACHE PATH "Where re, LibreSSL and the WebRTC bridge are installed (native.ps1's prefix)")
set(BARESIP_SOURCE "" CACHE PATH "baresip's sources, for its headers")
foreach(required KSIP_NATIVE BARESIP_SOURCE)
  if(NOT ${required})
    message(FATAL_ERROR "${required} is not set")
  endif()
endforeach()

# libre's compile definitions, as baresip and its modules get them: what its
# headers lay out (atomics, threads, the TLS backend) depends on them. STATIC
# is baresip's: DECL_EXPORTS(name) is then exports_name, the symbol its
# static module table names.
find_package(re CONFIG REQUIRED HINTS "${KSIP_NATIVE}/lib/cmake/re")
add_compile_definitions(${RE_DEFINITIONS} STATIC)
if(NOT OPENSSL_INCLUDE_DIR)
  set(OPENSSL_INCLUDE_DIR "${KSIP_NATIVE}/include")
endif()
include_directories("${BARESIP_SOURCE}/include" "${KSIP_NATIVE}/include/re")

add_subdirectory(ksip)
add_subdirectory(ksip_ctrl)
add_subdirectory(ksip_audio)
add_subdirectory(ksip_audio/filter ksip_audio_filter)
add_library(ksip_modules STATIC
  $<TARGET_OBJECTS:ksip>
  $<TARGET_OBJECTS:ksip_ctrl>
  $<TARGET_OBJECTS:ksip_audio>
  $<TARGET_OBJECTS:ksip_audio_filter>)

# What the app calls directly, outside baresip (app/): the CRT streams of the
# engine process bound to the app's pipes before baresip's main runs, and
# the certificate check through LibreSSL, in the app's own process.
add_subdirectory(app)

install(TARGETS ksip_modules ksip_app ARCHIVE DESTINATION lib)
