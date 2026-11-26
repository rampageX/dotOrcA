#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <cui/cui.h>
#include <cui/cui_app.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

extern void arena_worker_start(void);
extern int arena_worker_status(char *out, size_t capacity);

typedef struct {
    char **paths;
    size_t count;
    int font_attempted;
    int font_ok;
    int drop_error;
} probe;

static void clear_paths(probe *p) {
    for (size_t i = 0; i < p->count; ++i) free(p->paths[i]);
    free(p->paths);
    p->paths = NULL;
    p->count = 0;
    p->drop_error = 0;
}

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

static void collect_drop(cui_ctx *ui, probe *p) {
    int count = cui_dropped_file_count(ui);
    if (count <= 0) return;
    clear_paths(p);
    p->paths = (char **)calloc((size_t)count, sizeof(*p->paths));
    if (!p->paths) { p->drop_error = 1; return; }
    for (int i = 0; i < count; ++i) {
        const char *s = cui_dropped_file(ui, i);
        if (!s) { p->drop_error = 1; continue; }
        size_t len = strlen(s) + 1;
        char *copy = (char *)malloc(len);
        if (!copy) { p->drop_error = 1; continue; }
        memcpy(copy, s, len);
        p->paths[p->count++] = copy;
    }
}

static void frame(cui_ctx *ui, void *user) {
    probe *p = (probe *)user;
    if (!p->font_attempted) {
        p->font_attempted = 1;
        p->font_ok = load_system_cjk(ui);
    }
    collect_drop(ui, p);
    char status[256] = {0};
    int busy = arena_worker_status(status, sizeof(status));
    cui_column(ui, .width = CUI_FILL, .height = CUI_FILL, .pad = 16, .gap = 12, .bg = CUI_COLOR_BACKGROUND) {
        cui_heading(ui, "dotOrcA | CUI S00", 2);
        cui_secondary(ui, p->font_ok ? "Software renderer / 软件渲染" : "Software renderer / CJK font unavailable");
        cui_text(ui, status);
        cui_scroll(ui, .id = "paths", .width = CUI_FILL, .height = CUI_FILL, .pad = 10, .gap = 8) {
            if (!p->count) cui_secondary(ui, p->font_ok ? "Drop files here / 拖入文件" : "Drop files here");
            for (size_t i = 0; i < p->count; ++i) {
                cui_push_id_int(ui, (int)i);
                cui_text(ui, p->paths[i]);
                cui_pop_id(ui);
            }
            if (p->drop_error) cui_text(ui, "Error: one or more paths could not be stored.");
        }
        cui_row(ui, .gap = 8) {
            if (cui_button(ui, p->font_ok ? "5s worker test / 后台测试" : "5s worker test", .variant = CUI_BUTTON_PRIMARY)) {
                arena_worker_start();
                busy = 1;
                cui_request_redraw(ui);
            }
            if (cui_button(ui, p->font_ok ? "Clear / 清空" : "Clear")) {
                clear_paths(p);
                cui_request_redraw(ui);
            }
        }
    }
    if (busy) cui_request_wakeup(ui, 0.1f);
}

int arena_cui_run(void) {
    probe p = {0};
    // Force software regardless of an inherited renderer environment override.
    _putenv_s("CUI_RENDERER", "software");
    int result = cui_app_run(&(cui_app_desc){
        .title = "dotOrcA S00 | CUI software",
        .width = 720, .height = 400,
        .theme = CUI_THEME_DARK,
        .renderer = CUI_RENDERER_SOFTWARE,
        .continuous = false
    }, frame, &p);
    clear_paths(&p);
    return result;
}
