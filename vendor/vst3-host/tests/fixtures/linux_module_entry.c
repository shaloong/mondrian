/* Owned ELF ABI probe. No third-party plugin or DSP compatibility claim. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdbool.h>
#include <stddef.h>

static bool entry_matched;

bool ModuleEntry(void *provided_handle)
{
    Dl_info info;
    if (!dladdr((void *)&ModuleEntry, &info))
        return false;
    void *actual_handle = dlopen(info.dli_fname, RTLD_NOW | RTLD_NOLOAD);
    entry_matched = actual_handle && provided_handle == actual_handle;
    if (actual_handle)
        dlclose(actual_handle);
    return entry_matched;
}

bool ModuleExit(void) { return entry_matched; }
void *GetPluginFactory(void) { return NULL; }
bool MondrianEntryMatched(void) { return entry_matched; }
