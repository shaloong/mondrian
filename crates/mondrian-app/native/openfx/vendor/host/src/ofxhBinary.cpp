// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause

#include "ofxhBinary.h"

using namespace OFX;

#if defined(_WIN32)
namespace {
std::wstring utf8ToWide(const std::string& text) {
  const int length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                                        text.c_str(), -1, nullptr, 0);
  if (length == 0) { return {}; }
  std::wstring wide(static_cast<size_t>(length), L'\0');
  if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.c_str(), -1,
                          wide.data(), length) == 0) { return {}; }
  wide.pop_back();
  return wide;
}
}
#endif

Binary::Binary(const std::string &binaryPath): _binaryPath(binaryPath), _invalid(false), _dlHandle(0), _users(0)
{
#if defined(_WIN32)
  struct _stat64 sb;
  const std::wstring widePath = utf8ToWide(binaryPath);
  const bool missing = widePath.empty() || _wstat64(widePath.c_str(), &sb) != 0;
#else
  struct stat sb;
  const bool missing = stat(binaryPath.c_str(), &sb) != 0;
#endif
  if (missing) {
    _invalid = true;
    _time = 0;
    _size = 0;
  }
  else {
    _time = sb.st_mtime;
    _size = sb.st_size;
  }
}


// actually open the binary.
void Binary::load()
{
  if(_invalid)
    return;

#if defined (UNIX)
  _dlHandle = dlopen(_binaryPath.c_str(), RTLD_LAZY|RTLD_LOCAL);
#else
  const std::wstring widePath = utf8ToWide(_binaryPath);
  _dlHandle = widePath.empty() ? nullptr : LoadLibraryW(widePath.c_str());
#endif
  if (_dlHandle == 0) {
#if defined (UNIX)
    std::cerr << "couldn't open library " << _binaryPath << " because " << dlerror() << std::endl;
#else
    LPVOID lpMsgBuf = NULL;
    DWORD err = GetLastError();

    FormatMessageA(FORMAT_MESSAGE_ALLOCATE_BUFFER |
      FORMAT_MESSAGE_FROM_SYSTEM |
      FORMAT_MESSAGE_IGNORE_INSERTS,
      NULL,
      err,
      MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT),
      (LPSTR) &lpMsgBuf,
      0, NULL);

    std::cerr << "couldn't open library " << _binaryPath << " because " << (char*)lpMsgBuf << " was returned" << std::endl;
    if (lpMsgBuf != NULL) {
      LocalFree(lpMsgBuf);
    }
#endif
    _invalid = true;
  }
}

/// close the binary
void Binary::unload() {
  if (_dlHandle != 0) {
#if defined (UNIX)
    dlclose(_dlHandle);
#elif defined (_WIN32)
    FreeLibrary(_dlHandle);
#endif
    _dlHandle = 0;
  }
}

/// look up a symbol in the binary file and return it as a pointer.
/// returns null pointer if not found, or if the library is not loaded.
void *Binary::findSymbol(const std::string &symbol) {
  if (_dlHandle != 0) {
#if defined(UNIX)
    return dlsym(_dlHandle, symbol.c_str());
#elif defined (_WIN32)
    return (void*)GetProcAddress(_dlHandle, symbol.c_str());
#endif
  } else {
    return 0;
  }
}


void Binary::ref()
{
  if (_users == 0) {
    load();
  }
  _users++;
}

void Binary::unref()
{
  _users--;
  if (_users == 0) {
    unload();
  }
  if (_users < 0) {
    _users = 0;
  }
}
