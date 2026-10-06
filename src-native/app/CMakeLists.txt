# What the app (ksip.exe) calls directly, not through baresip: stdio.cpp runs
# in the engine process before baresip's main, binding the CRT streams to the
# pipes the app hands over; trust.cpp runs in the app's own process, asking
# LibreSSL (which the engine links) whether a Windows certificate parses.
# One archive, ksip_app.lib, which the app links (src-tauri/build.rs).
project(ksip_app)
add_library(${PROJECT_NAME} STATIC stdio.cpp trust.cpp)
target_compile_features(${PROJECT_NAME} PRIVATE cxx_std_20)
target_compile_options(${PROJECT_NAME} PRIVATE /utf-8)
target_include_directories(${PROJECT_NAME} SYSTEM PRIVATE "${KSIP_NATIVE}/include")
