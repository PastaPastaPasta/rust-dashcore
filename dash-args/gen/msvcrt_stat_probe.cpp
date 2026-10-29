// msvcrt _wstat's st_mode and st_dev, which libstdc++'s Windows std::filesystem::equivalent
// compares before the files' volume serial and index (dash-args src/paths.rs). Run in an empty
// scratch directory on Windows or Wine; it creates the files it stats. Core's releases link
// msvcrt.dll, so its _wstat64 is called (a MinGW toolchain may default to UCRT, which differs).
#include <sys/stat.h>
#include <windows.h>
#include <cstdio>
#include <string>
static void make(const std::wstring& name) {
    FILE* f = _wfopen(name.c_str(), L"w");
    if (f) fclose(f);
}
using wstat64_fn = int(__cdecl*)(const wchar_t*, struct _stat64*);
static wstat64_fn msvcrt_wstat64;
static void show(const std::wstring& name) {
    struct _stat64 st;
    if (msvcrt_wstat64(name.c_str(), &st) == 0) {
        printf("%ls\t%o\t%d\n", name.c_str(), (unsigned)st.st_mode, (int)st.st_dev);
    } else {
        printf("%ls\terror\n", name.c_str());
    }
}
int main() {
    msvcrt_wstat64 = reinterpret_cast<wstat64_fn>(
        reinterpret_cast<void*>(GetProcAddress(LoadLibraryW(L"msvcrt.dll"), "_wstat64")));
    if (!msvcrt_wstat64) return 1;
    wchar_t cwd[MAX_PATH];
    GetCurrentDirectoryW(MAX_PATH, cwd);
    std::wstring dir = cwd;
    printf("cwd\t%ls\n", cwd);
    const wchar_t* files[] = {L"a.conf", L"a.exe", L"b.EXE", L"a.bat", L"a.cmd", L"a.com",
                              L"a.exe.txt", L"a.exex", L"exe", L"ro.conf"};
    for (auto f : files) make(f);
    CreateDirectoryW(L"d.exe", nullptr);
    CreateDirectoryW(L"d", nullptr);
    SetFileAttributesW(L"ro.conf", FILE_ATTRIBUTE_READONLY);
    for (auto f : files) show(f);
    for (auto f : files) show(dir + L"\\" + f);
    show(L"x\\..\\a.exe");
    show(L"d");
    show(L"d.exe");
    show(dir + L"\\d");
    show(dir + L"\\a.conf");
    std::wstring lower = dir;
    lower[0] = towlower(lower[0]);
    show(lower + L"\\a.conf");
    show(L"C:\\windows");
    SetFileAttributesW(L"ro.conf", FILE_ATTRIBUTE_NORMAL);
}
