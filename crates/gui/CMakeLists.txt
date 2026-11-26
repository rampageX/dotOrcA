cmake_minimum_required(VERSION 3.21)
project(arena_cui LANGUAGES C)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY_RELEASE "${CMAKE_BINARY_DIR}/lib")
set(CUI_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(CUI_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(CUI_BUILD_SHARED OFF CACHE BOOL "" FORCE)
set(CUI_D3D12 OFF CACHE BOOL "" FORCE)
include(FetchContent)
FetchContent_Declare(cui
  GIT_REPOSITORY https://github.com/yappologistic/CUI.git
  GIT_TAG f9597a30d3bba4f7ef3e0f918e31c28bdcd4d2a3
)
FetchContent_MakeAvailable(cui)
add_library(cui_bridge STATIC bridge.c)
target_compile_features(cui_bridge PRIVATE c_std_99)
target_link_libraries(cui_bridge PRIVATE cui::cui)
if(MSVC)
  target_compile_options(cui_bridge PRIVATE /utf-8 /Gy /Gw)
endif()

# Link the resource directly into the executable, not through a static archive.
if(MSVC)
  enable_language(RC)
  add_custom_command(
    OUTPUT "${CMAKE_BINARY_DIR}/lib/dotorca.res"
    COMMAND ${CMAKE_COMMAND} -E make_directory "${CMAKE_BINARY_DIR}/lib"
    COMMAND "${CMAKE_RC_COMPILER}" /nologo /fo "${CMAKE_BINARY_DIR}/lib/dotorca.res" "${CMAKE_CURRENT_SOURCE_DIR}/dotorca.rc"
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    DEPENDS dotorca.rc dotorca.ico
    VERBATIM
  )
  add_custom_target(dotorca_resource ALL DEPENDS "${CMAKE_BINARY_DIR}/lib/dotorca.res")
endif()
