# The Windows 11 virtual camera; docs/building.md says how to build it
cmake_minimum_required(VERSION 3.20)
project(TelescopeCamera CXX RC)

if(NOT MSVC)
    message(FATAL_ERROR "The camera DLL builds with MSVC only")
endif()

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
# The C++ runtime goes into the DLL, so Frame Server doesn't need the VC++ redistributable to load it
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

# The wait screen, rendered by the app's own code (scripts/write_camera_wait_screen.py) and built in as a resource
find_package(Python3 REQUIRED COMPONENTS Interpreter)
set(WAIT_SCREEN ${CMAKE_CURRENT_BINARY_DIR}/wait_screen.bin)
add_custom_command(
    OUTPUT ${WAIT_SCREEN}
    COMMAND Python3::Interpreter ${CMAKE_CURRENT_SOURCE_DIR}/../scripts/write_camera_wait_screen.py ${WAIT_SCREEN}
    DEPENDS ../scripts/write_camera_wait_screen.py ../telescope/vcam.py ../telescope/theme.py
    COMMENT "Rendering the camera's wait screen"
)
# 101 is kWaitScreenResource in wait_screen.h
file(WRITE ${CMAKE_CURRENT_BINARY_DIR}/wait_screen.rc "101 RCDATA \"${WAIT_SCREEN}\"\n")
set_source_files_properties(${CMAKE_CURRENT_BINARY_DIR}/wait_screen.rc PROPERTIES OBJECT_DEPENDS ${WAIT_SCREEN})

add_library(TelescopeCamera SHARED
    dllmain.cpp
    media_source.cpp
    shared_frame.cpp
    wait_screen.cpp
    ${CMAKE_CURRENT_BINARY_DIR}/wait_screen.rc
    ${WAIT_SCREEN}
    TelescopeCamera.def
)
target_compile_definitions(TelescopeCamera PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX _WIN32_WINNT=0x0A00)
target_compile_options(TelescopeCamera PRIVATE /W4 /WX /permissive- /guard:cf)
# mfsensorgroup.dll is only loaded once a camera is set up, so the DLL itself still loads on older Windows
target_link_libraries(TelescopeCamera PRIVATE mfplat mfuuid mfsensorgroup advapi32 ole32 delayimp)
target_link_options(TelescopeCamera PRIVATE /DELAYLOAD:mfsensorgroup.dll /guard:cf /DYNAMICBASE /NXCOMPAT)
