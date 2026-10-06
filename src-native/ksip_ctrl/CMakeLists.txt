# The engine's side of the app's links to it: the authenticated control
# connection (ksip_ctrl.cpp, the module) and the engine's stdout and stderr
# bound to the app's pipes (stdio.cpp, which the app calls before baresip's
# main). Objects of ksip_modules.lib (../CMakeLists.txt).
project(ksip_ctrl)
add_library(${PROJECT_NAME} OBJECT ksip_ctrl.cpp stdio.cpp)
target_compile_features(${PROJECT_NAME} PRIVATE cxx_std_20)
target_compile_definitions(${PROJECT_NAME} PRIVATE NOMINMAX)
target_compile_options(${PROJECT_NAME} PRIVATE /utf-8)
