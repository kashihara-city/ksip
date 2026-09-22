#include <windows.h>
#include <io.h>
#include <fcntl.h>
#include <cstdio>
#include <initializer_list>
extern "C" void ksip_stdio_init() {
    // GUI-subsystem executables have no CRT streams by default. Preserve the
    // inherited pipe handles while binding stdout/stderr for the engine.
    for (auto pair : {0, 1}) {
        DWORD which = pair ? STD_ERROR_HANDLE : STD_OUTPUT_HANDLE;
        HANDLE copy = nullptr;
        HANDLE source = GetStdHandle(which);
        if (!source || source == INVALID_HANDLE_VALUE || !DuplicateHandle(GetCurrentProcess(), source,
            GetCurrentProcess(), &copy, 0, FALSE, DUPLICATE_SAME_ACCESS)) continue;
        FILE *stream = pair ? stderr : stdout;
        FILE *opened = nullptr;
        if (freopen_s(&opened, "NUL", "w", stream)) { CloseHandle(copy); continue; }
        int fd = _open_osfhandle(reinterpret_cast<intptr_t>(copy), _O_WRONLY | _O_BINARY);
        if (fd < 0) { CloseHandle(copy); continue; }
        _dup2(fd, _fileno(stream));
        _close(fd);
        setvbuf(stream, nullptr, _IONBF, 0);
    }
}
