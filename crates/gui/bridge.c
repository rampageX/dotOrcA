#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shlobj.h>
#include <cui/cui.h>
#include <cui/cui_app.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

/* Input is borrowed, length-delimited UTF-8. Output is caller-owned and
   NUL terminated. Negative returns indicate an ABI or callback failure. */
typedef struct {
    uint32_t id;
    uint32_t flags;
    int32_t percent;
    char name[1024];
    char status[512];
} orca_job_row;

extern int32_t orca_add_path(const char *path, size_t path_len);
extern int32_t orca_start(const char *size, size_t size_len, int32_t mode, int32_t codec, uint32_t edge);
extern int32_t orca_action(uint32_t action, uint32_t id);
extern int32_t orca_frame(char *out, size_t capacity, uint32_t *count);
extern int32_t orca_row(uint32_t index, orca_job_row *out, size_t capacity);
extern int32_t orca_detail(uint32_t index, char *out, size_t capacity);

enum { CANCEL = 1, CLEAR_FINISHED, CLEAR_ALL };
static const uint32_t edges[] = { 3840, 2560, 1920, 1280 };

typedef struct {
    char mib[24];
    char percent[24];
    int target_mode;
    int codec;
    int resolution;
    uint32_t expanded;
    int font_pending;
    int font_tried;
    int font_ok;
    int callback_error;
    int icon_tried;
    bool confirm_clear_all;
} window_state;

/* No font bundled: read a local Windows font by a wide filesystem path.
   File bytes are copied by CUI; the transient buffer is then freed. */
static int load_system_cjk(cui_ctx *ui) {
    static const wchar_t *names[] = { L"\\Fonts\\simhei.ttf", L"\\Fonts\\msyh.ttc", L"\\Fonts\\simsun.ttc" };
    wchar_t dir[MAX_PATH], path[MAX_PATH];
    UINT n = GetWindowsDirectoryW(dir, MAX_PATH);
    if (!n || n >= MAX_PATH) return 0;
    for (size_t i = 0; i < sizeof(names)/sizeof(names[0]); ++i) {
        if (n + wcslen(names[i]) >= MAX_PATH) continue;
        wcscpy(path, dir); wcscat(path, names[i]);
        FILE *f = _wfopen(path, L"rb");
        if (!f) continue;
        if (fseek(f, 0, SEEK_END) != 0) { fclose(f); continue; }
        long len = ftell(f);
        if (len <= 0 || len > 64L*1024*1024 || fseek(f, 0, SEEK_SET) != 0) { fclose(f); continue; }
        unsigned char *buf = (unsigned char *)malloc((size_t)len);
        if (!buf) { fclose(f); continue; }
        size_t read = fread(buf, 1, (size_t)len, f);
        fclose(f);
        cui_font font = read == (size_t)len ? cui_font_load_memory(ui, buf, read, true) : -1;
        free(buf);
        if (font >= 0) {
            cui_font_add_fallback(ui, CUI_FONT_BODY, font);
            cui_font_add_fallback(ui, CUI_FONT_MEDIUM, font);
            cui_font_add_fallback(ui, CUI_FONT_DISPLAY, font);
            return 1;
        }
    }
    return 0;
}


static void collect_drop(cui_ctx *ui, window_state *window) {
    int count = cui_dropped_file_count(ui);
    if (count <= 0) return;
    window->callback_error = 0;
    for (int i = 0; i < count; ++i) {
        const char *path = cui_dropped_file(ui, i);
        if (!path || orca_add_path(path, strlen(path)) < 0) {
            window->callback_error = 1;
        }
    }
}

static void action(cui_ctx *ui, window_state *window, uint32_t command, uint32_t id) {
    window->callback_error = orca_action(command, id) < 0;
    cui_request_redraw(ui);
}

static void draw_job(cui_ctx *ui, window_state *window, uint32_t index) {
    orca_job_row job;
    if (orca_row(index, &job, sizeof(job)) != 1) {
        window->callback_error = 1;
        return;
    }

    cui_push_id_int(ui, (int)job.id);
    cui_column(ui, .width = CUI_FILL, .pad = 12, .gap = 6, .bg = CUI_COLOR_SURFACE) {
        cui_row(ui, .width = CUI_FILL, .gap = 8) {
            cui_text(ui, job.name, .width = CUI_FILL, .nowrap = true, .font = CUI_FONT_MEDIUM);
            if (cui_button(ui, window->expanded == job.id ? "Hide details" : "Details")) {
                window->expanded = window->expanded == job.id ? 0 : job.id;
                cui_request_redraw(ui);
            }
            if (cui_button(ui, "Cancel", .disabled = (job.flags & 1u) == 0)) {
                action(ui, window, CANCEL, job.id);
            }
        }
        cui_secondary(ui, job.status);
        if (job.percent >= 0) {
            cui_progress(ui, (float)job.percent / 100.0f, .width = CUI_FILL,
                         .label = "Encode pass", .show_value = true);
        }
        if (window->expanded == job.id) {
            char detail[49152];
            if (orca_detail(index, detail, sizeof(detail)) == 1) {
                cui_text(ui, detail, .width = CUI_FILL, .selectable = true);
            }
        }
    }
    cui_pop_id(ui);
}

/* Shared resource handles remain valid for the life of the module. */
static int apply_window_icon(HWND hwnd) {
    if (!hwnd) return 0;
    HINSTANCE module = GetModuleHandleW(NULL);
    HICON big = (HICON)LoadImageW(module, MAKEINTRESOURCEW(101), IMAGE_ICON,
        GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON), LR_SHARED);
    HICON small_icon = (HICON)LoadImageW(module, MAKEINTRESOURCEW(101), IMAGE_ICON,
        GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON), LR_SHARED);
    if (!big || !small_icon) return 0;
    SendMessageW(hwnd, WM_SETICON, ICON_BIG, (LPARAM)big);
    SendMessageW(hwnd, WM_SETICON, ICON_SMALL, (LPARAM)small_icon);
    return SendMessageW(hwnd, WM_GETICON, ICON_BIG, 0) == (LRESULT)big &&
           SendMessageW(hwnd, WM_GETICON, ICON_SMALL, 0) == (LRESULT)small_icon;
}

static void frame(cui_ctx *ui, void *user) {
    static const char *modes[] = { "Fixed size", "% of source" };
    static const char *codecs[] = { "H.264", "HEVC", "AV1" };
    static const char *resolutions[] = { "3840 px", "2560 px", "1920 px", "1280 px" };
    window_state *window = (window_state *)user;
    if (!window->icon_tried) {
        window->icon_tried = 1;
        apply_window_icon((HWND)cui_app_native_window(NULL));
    }
    if (window->font_pending && !window->font_tried) {
        window->font_tried = 1;
        window->font_ok = load_system_cjk(ui);
    } else if (!window->font_pending) {
        window->font_pending = 1;
        cui_request_wakeup(ui, 0.05f);
    }

    if (!window->confirm_clear_all) collect_drop(ui, window);
    uint32_t count = 0;
    char status[768] = {0};
    int busy = orca_frame(status, sizeof(status), &count);
    cui_column(ui, .width = CUI_FILL, .height = CUI_FILL, .pad = 20, .gap = 12,
               .bg = CUI_COLOR_BACKGROUND) {
        cui_heading(ui, "dotOrcA", 1);
        cui_row(ui, .width = CUI_FILL, .gap = 12) {
            cui_select(ui, "target-mode", modes, 2, &window->target_mode,
                       .label = "Capacity mode", .width = CUI_FILL);
            if (window->target_mode == 0) {
                cui_input(ui, "mib", window->mib, sizeof(window->mib),
                          .label = "Capacity per file", .suffix = "MiB", .width = CUI_FILL);
            } else {
                cui_input(ui, "percent", window->percent, sizeof(window->percent),
                          .label = "Output / source", .suffix = "%", .width = CUI_FILL);
            }
            cui_select(ui, "codec", codecs, 3, &window->codec,
                       .label = "Output codec", .width = CUI_FILL);
            cui_select(ui, "max-edge", resolutions, 4, &window->resolution,
                       .label = "Max. long edge", .width = CUI_FILL);
        }
        cui_secondary(ui, window->target_mode == 0
            ? "Files already within the MiB limit are skipped. Settings apply on Start."
            : "50% means half the original file size. Each file gets its own limit. Settings apply on Start.");
        cui_column(ui, .width = CUI_FILL, .pad = 10, .bg = CUI_COLOR_SURFACE_RAISED) {
            cui_text(ui, "Drop video files anywhere in this window (up to 64 per drop).");
        }
        if (window->font_tried && !window->font_ok) {
            cui_secondary(ui, "CJK font unavailable. Some filename characters may be missing on screen.");
        }
        cui_scroll(ui, .id = "queue", .width = CUI_FILL, .height = CUI_FILL, .gap = 8) {
            for (uint32_t i = 0; i < count; ++i) draw_job(ui, window, i);
        }
        cui_text(ui, busy < 0 || window->callback_error ? "Queue interface error." : status, .width = CUI_FILL);
        cui_row(ui, .width = CUI_FILL, .gap = 8) {
            if (cui_button(ui, "Start queued", .variant = CUI_BUTTON_PRIMARY, .disabled = !count)) {
                const char *value = window->target_mode == 0 ? window->mib : window->percent;
                window->callback_error = orca_start(value, strlen(value), window->target_mode,
                    window->codec, edges[window->resolution]) < 0;
                cui_request_redraw(ui);
            }
            if (cui_button(ui, "Cancel all", .disabled = !count)) action(ui, window, CANCEL, 0);
            if (cui_button(ui, "Clear finished", .disabled = !count)) {
                window->expanded = 0;
                action(ui, window, CLEAR_FINISHED, 0);
            }
            if (cui_button(ui, "Clear All", .disabled = !count)) {
                window->confirm_clear_all = true;
                cui_request_redraw(ui);
            }
        }
    }
    if (window->confirm_clear_all) {
        cui_dialog(ui, "Clear All", &window->confirm_clear_all,
                   .width = 440, .description = "Stop active tasks and clear the entire queue?") {
            cui_text(ui, "Source files and saved outputs will not be deleted.", .width = CUI_FILL);
            cui_dialog_footer(ui) {
                if (cui_button(ui, "Keep tasks")) {
                    window->confirm_clear_all = false;
                    cui_request_redraw(ui);
                }
                if (cui_button(ui, "Clear All", .variant = CUI_BUTTON_PRIMARY)) {
                    window->confirm_clear_all = false;
                    window->expanded = 0;
                    action(ui, window, CLEAR_ALL, 0);
                }
            }
        }
    }
    if (busy > 0) cui_request_wakeup(ui, 0.1f);
}

static cui_app_desc description(void) {
    _putenv_s("CUI_RENDERER", "software");
    return (cui_app_desc){
        .title = "dotOrcA", .width = 880, .height = 640,
        .theme = CUI_THEME_DARK, .renderer = CUI_RENDERER_SOFTWARE, .continuous = false
    };
}

int orca_cui_run(void) {
    window_state window = { .mib = "100", .percent = "50", .codec = 1, .resolution = 0 };
    cui_app_desc desc = description();
    return cui_app_run(&desc, frame, &window);
}

/* Exercise the same Win32 UTF-16 WM_DROPFILES handler and CUI callback
   used by Explorer. The files need not exist for admission to the queue. */
static int post_test_drop(HWND hwnd) {
    static const wchar_t names[] =
        L"C:\\Test & ( ) ^ % !\\小K 的鹅腿坟场 & (黑洞) ^ % !.avi\0"
        L"C:\\Test & ( ) ^ % !\\-movie \xD83C" L"\xDFAC.mp4\0";
    HGLOBAL payload = GlobalAlloc(GHND, sizeof(DROPFILES) + sizeof(names));
    if (!payload) return 0;
    DROPFILES *drop = (DROPFILES *)GlobalLock(payload);
    if (!drop) { GlobalFree(payload); return 0; }
    drop->pFiles = sizeof(DROPFILES);
    drop->pt.x = 30;
    drop->pt.y = 30;
    drop->fWide = TRUE;
    memcpy((char *)drop + sizeof(DROPFILES), names, sizeof(names));
    GlobalUnlock(payload);
    if (!PostMessageW(hwnd, WM_DROPFILES, (WPARAM)payload, 0)) {
        GlobalFree(payload);
        return 0;
    }
    return 1;
}

int orca_cui_self_test(void) {
    cui_app_desc desc = description();
    desc.hidden = true;
    cui_app *app = cui_app_create(&desc);
    if (!app) return 10;
    int result = 15;
    if (!apply_window_icon((HWND)cui_app_native_window(app))) {
        cui_app_destroy(app);
        return result;
    }
    result = 11;
    window_state window = { .mib = "100", .percent = "50", .codec = 1, .font_pending = 1, .font_tried = 1, .icon_tried = 1 };
    if (!cui_app_is_software(app) || !cui_app_frame(app)) goto done;
    cui_ctx *ui = cui_app_ui(app);
    frame(ui, &window);
    if (!post_test_drop((HWND)cui_app_native_window(app)) || !cui_app_frame(app)) goto done;
    frame(ui, &window);
    char status[768];
    uint32_t count = 0;
    orca_job_row a, b;
    result = 12;
    if (orca_frame(status, sizeof(status), &count) < 0 || count != 2 ||
        orca_row(0, &a, sizeof(a)) != 1 || orca_row(1, &b, sizeof(b)) != 1 ||
        strcmp(a.name, "小K 的鹅腿坟场 & (黑洞) ^ % !.avi") ||
        strcmp(b.name, "-movie 🎬.mp4")) goto done;
    result = 13;
    if (orca_action(CANCEL, b.id) != 1 || orca_frame(status, sizeof(status), &count) < 0 ||
        !strstr(status, "1 cancelled")) goto done;
    if (orca_action(CLEAR_FINISHED, 0) != 1 || orca_frame(status, sizeof(status), &count) < 0 ||
        count != 1) goto done;
    result = 16;
    /* Render the modal in the normal frame path; opening/dismissing it
       must not clear the queue. The positive action is covered below. */
    window.confirm_clear_all = true;
    if (!cui_app_frame(app)) goto done;
    frame(ui, &window);
    if (orca_frame(status, sizeof(status), &count) < 0 || count != 1) goto done;
    window.confirm_clear_all = false;
    if (!cui_app_frame(app)) goto done;
    frame(ui, &window);
    if (orca_frame(status, sizeof(status), &count) < 0 || count != 1) goto done;
    result = 14;
    if (orca_action(CLEAR_ALL, 0) != 1 || orca_frame(status, sizeof(status), &count) < 0 ||
        count != 0) goto done;
    result = 0;
done:
    cui_app_destroy(app);
    return result;
}
