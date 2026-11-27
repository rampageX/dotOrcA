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
#include "labels.h"

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
extern int32_t orca_detail(uint32_t id, uint32_t section, char *out, size_t capacity);
extern int32_t orca_ui_init(int32_t system_zh);
extern int32_t orca_set_language(int32_t zh);
typedef struct { int32_t x,y,width,height,state,has_position; } orca_placement;
extern int32_t orca_load_placement(int32_t kind, orca_placement *out);
extern int32_t orca_save_placement(int32_t kind, const orca_placement *value);
typedef struct { char mib[24],percent[24]; int32_t mode,codec,resolution; } orca_settings;
extern int32_t orca_load_settings(orca_settings *out);
extern int32_t orca_save_settings(const orca_settings *value);
extern int32_t orca_get_directory(int32_t kind,char *out,size_t cap);
extern int32_t orca_set_directories(const char *output,size_t on,const char *tools,size_t tn);
#define DIR_CAP 131072
#define ORCA_ADVANCED 0x1ff0
#define DETAIL_CAP 131072
typedef struct {
    cui_app *app;
    uint32_t id;
    int language;
    int font_ok;
    char sections[5][DETAIL_CAP];
    char scratch[DETAIL_CAP];
    char all[DETAIL_CAP * 2];
} detail_state;

enum { CANCEL = 1, CLEAR_FINISHED, CLEAR_ALL, RESET };
static const uint32_t edges[] = { 3840, 2560, 1920, 1280 };

typedef struct {
    char mib[24];
    char percent[24];
    int target_mode;
    int codec;
    int resolution;
    cui_app *main_app;
    detail_state *detail;
    int language;
    int font_pending;
    int font_tried;
    int font_ok;
    int callback_error;
    int icon_tried;
    bool confirm_clear_all;
    bool advanced;
    bool advanced_error;
    char output_dir[DIR_CAP],tools_dir[DIR_CAP];
    WNDPROC old_proc;
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
            cui_font_add_fallback(ui, CUI_FONT_MONO, font);
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


static int apply_window_icon(HWND hwnd);
static void close_detail(window_state *window) {
    detail_state *d = window->detail;
    if (!d) return;
    cui_window_placement p;
    if (cui_app_placement(d->app, &p)) {
        orca_placement saved = {p.x,p.y,p.width,p.height,p.state == CUI_WINDOW_MAXIMIZED,p.has_position};
        orca_save_placement(1,&saved);
    }
    cui_app_destroy(d->app);
    free(d);
    window->detail = NULL;
}
static int open_detail(window_state *window, uint32_t id) {
    if (window->detail) {
        if (window->detail->id != id) {
            window->detail->id = id;
            memset(window->detail->sections, 0, sizeof(window->detail->sections));
        }
        HWND existing=(HWND)cui_app_native_window(window->detail->app);
        if (IsIconic(existing)) ShowWindow(existing,SW_RESTORE);
        SetForegroundWindow((HWND)cui_app_native_window(window->detail->app));
        return 1;
    }
    detail_state *d = (detail_state *)calloc(1,sizeof(*d));
    if (!d) return 0;
    orca_placement saved;
    cui_window_placement placement = {0};
    if (orca_load_placement(1,&saved) == 1) {
        placement.x=saved.x; placement.y=saved.y;
        placement.width=saved.width; placement.height=saved.height;
        placement.state=saved.state ? CUI_WINDOW_MAXIMIZED : CUI_WINDOW_NORMAL;
        placement.has_position=saved.has_position != 0;
    }
    cui_app_desc desc = {.title=orca_label(L_DETAILS), .width=740, .height=640,
        .min_width=480, .min_height=340, .theme=CUI_THEME_DARK,
        .renderer=CUI_RENDERER_SOFTWARE, .continuous=false,
        .placement=placement.width ? &placement : NULL};
    d->app = cui_app_create(&desc);
    if (!d->app) { free(d); return 0; }
    d->id=id; d->language=window->language;
    HWND hwnd=(HWND)cui_app_native_window(d->app);
    DragAcceptFiles(hwnd,FALSE);
    apply_window_icon(hwnd);
    d->font_ok=load_system_cjk(cui_app_ui(d->app));
    window->detail=d;
    return 1;
}
/* Both app contexts are pumped on the GUI thread. No nested modal loop.
   Main snapshots are refreshed first; the detail view only reads by job ID. */
static float detail_height(cui_ctx *ui,const char *text,float max_height) {
    float width=cui_viewport_size(ui).x-88.0f;
    if(width<120.0f)width=120.0f;
    cui_theme *theme=cui_theme_get(ui);
    float size=theme->text_body*theme->font_scale;
    float lines=0;
    const char *p=text;
    while(*p && lines*size*1.5f<max_height) {
        const char *e=strchr(p,'\n');
        int len=e?(int)(e-p):(int)strlen(p);
        float w=cui_measure_text(ui,CUI_FONT_BODY,size,p,len).width;
        int n=(int)(w/width)+1;
        lines+=(float)n+(n>1?1.0f:0.0f);
        if(!e)break;
        p=e+1;
    }
    float h=lines*size*1.5f+24.0f;
    return h<44.0f?44.0f:(h>max_height?max_height:h);
}
static int prefix(const char *s,const char *p){return strncmp(s,p,strlen(p))==0;}
static cui_color line_color(cui_ctx *ui,uint32_t section,const char *s) {
    if(prefix(s,"Source file:")||prefix(s,"Source path:")||prefix(s,"源文件：")||
       prefix(s,"Destination:")||prefix(s,"目标文件：")||prefix(s,"Saved output:")||prefix(s,"已保存："))
        return cui_color_of(ui,CUI_COLOR_TEXT_SECONDARY);
    if(section==4)return cui_color_of(ui,CUI_COLOR_DANGER);
    if(prefix(s,"Video:")||prefix(s,"视频：")||prefix(s,"Planned output:")||prefix(s,"计划输出："))
        return CUI_RGBA(0x91d5ffff);
    if(prefix(s,"Color:")||prefix(s,"色彩："))return CUI_RGBA(0xe0b0ffff);
    if(prefix(s,"Audio:")||prefix(s,"音频：")||prefix(s,"Subtitle:")||prefix(s,"字幕："))
        return CUI_RGBA(0x8ce0ceff);
    if(section==2||section==3)return CUI_RGBA(0x9de8acff);
    if(section==1)return CUI_RGBA(0xffd38aff);
    return CUI_RGBA(0xb9d9ffff);
}
static void detail_field(cui_ctx *ui,uint32_t section,char *line,size_t cap,float max_height) {
    cui_theme *theme=cui_theme_get(ui);
    cui_color prev=theme->colors[CUI_TOKEN_FOREGROUND];
    theme->colors[CUI_TOKEN_FOREGROUND]=line_color(ui,section,line);
    cui_textarea(ui,"readonly",line,cap,.read_only=true,.width=CUI_FILL,.height=detail_height(ui,line,max_height));
    theme->colors[CUI_TOKEN_FOREGROUND]=prev;
}
static void tick_detail(window_state *window) {
    detail_state *d=window->detail;
    if (!d) return;
    if (orca_detail(d->id,0,d->scratch,sizeof(d->scratch)) != 1 || !cui_app_frame(d->app)) {
        close_detail(window);
        return;
    }
    if (d->language != window->language) {
        d->language=window->language;
        cui_app_set_title(d->app,orca_label(L_DETAILS));
    }
    cui_ctx *ui=cui_app_ui(d->app);
    const uint32_t titles[]={L_SOURCE,L_TARGET,L_OUTPUT,L_PROCESS,L_ERRORS};

    cui_column(ui,.width=CUI_FILL,.height=CUI_FILL,.pad=16,.gap=10,.bg=CUI_COLOR_BACKGROUND) {
        cui_heading(ui,orca_label(L_DETAILS),1);
        if (!d->font_ok) cui_secondary(ui,orca_label(L_FONT_ERROR));
        cui_scroll(ui,.id="detail-scroll",.width=CUI_FILL,.height=CUI_FILL,.gap=8) {
            cui_push_id_int(ui,(int)d->id);
            for (uint32_t i=0;i<5;++i) {
                if (orca_detail(d->id,i,d->scratch,sizeof(d->scratch)) == 1 &&
                    strcmp(d->scratch,d->sections[i]) != 0) {
                    memcpy(d->sections[i],d->scratch,strlen(d->scratch)+1);
                }
                cui_push_id_int(ui,(int)i);
                cui_text(ui,orca_label(titles[i]),.color=i==4?CUI_COLOR_DANGER:CUI_COLOR_ACCENT,.font=CUI_FONT_MEDIUM);
                if(i<3) {
                    cui_column(ui,.width=CUI_FILL,.gap=2,.pad=4,.bg=CUI_COLOR_SURFACE) {
                        char *line=d->sections[i];
                        for(int n=0;*line && n<64;++n) {
                            char *e=n==63?NULL:strchr(line,'\n');
                            if(e)*e=0;
                            cui_push_id_int(ui,n);
                            detail_field(ui,i,line,strlen(line)+1,240.0f);
                            cui_pop_id(ui);
                            if(!e)break;
                            *e='\n';line=e+1;
                        }
                    }
                } else {
                    detail_field(ui,i,d->sections[i],sizeof(d->sections[i]),420.0f);
                }
                cui_pop_id(ui);
            }
            cui_pop_id(ui);
        }
        cui_row(ui,.width=CUI_FILL,.gap=8) {
            if (cui_button(ui,orca_label(L_COPY)) &&
                orca_detail(d->id,5,d->all,sizeof(d->all)) == 1) {
                cui_set_clipboard_text(ui,d->all);
            }
            if (cui_button(ui,orca_label(L_CLOSE))) cui_app_quit(d->app);
        }
    }
    /* Do not block the main loop waiting for detail-window input. */
    cui_request_wakeup(ui,0.001f);
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
            if (cui_button(ui, orca_label(L_DETAILS))) {
                window->callback_error = !open_detail(window,job.id);
                cui_request_redraw(ui);
            }
            if (job.flags & 2u) {
                if (cui_button(ui, orca_label(L_RESET))) {
                    if (window->detail && window->detail->id == job.id) close_detail(window);
                    action(ui, window, RESET, job.id);
                }
            } else if (cui_button(ui, orca_label(L_CANCEL), .disabled = (job.flags & 1u) == 0)) {
                action(ui, window, CANCEL, job.id);
            }
        }
        cui_secondary(ui, job.status);
        if (job.percent >= 0) {
            cui_progress(ui, (float)job.percent / 100.0f, .width = CUI_FILL,
                         .label = orca_label(L_ENCODE_PASS), .show_value = true);
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

static void open_advanced(window_state *w) {
    orca_get_directory(0,w->output_dir,sizeof(w->output_dir));
    orca_get_directory(1,w->tools_dir,sizeof(w->tools_dir));
    w->advanced=true;w->advanced_error=false;
    cui_request_redraw(cui_app_ui(w->main_app));
}
static LRESULT CALLBACK main_proc(HWND hwnd,UINT msg,WPARAM wp,LPARAM lp) {
    window_state *w=(window_state*)GetPropW(hwnd,L"dotOrcA.window");
    if(w && msg==WM_SYSCOMMAND && (wp&0xfff0)==ORCA_ADVANCED) {
        open_advanced(w);return 0;
    }
    return w && w->old_proc?CallWindowProcW(w->old_proc,hwnd,msg,wp,lp):DefWindowProcW(hwnd,msg,wp,lp);
}
static void install_advanced(window_state *w) {
    HWND hwnd=(HWND)cui_app_native_window(w->main_app);
    SetPropW(hwnd,L"dotOrcA.window",w);
    w->old_proc=(WNDPROC)SetWindowLongPtrW(hwnd,GWLP_WNDPROC,(LONG_PTR)main_proc);
    HMENU menu=GetSystemMenu(hwnd,FALSE);
    wchar_t title[128];
    MultiByteToWideChar(CP_UTF8,0,orca_label(L_ADVANCED),-1,title,128);
    InsertMenuW(menu,SC_CLOSE,MF_BYCOMMAND|MF_STRING,ORCA_ADVANCED,title);
}
static void update_advanced_menu(window_state *w) {
    HWND hwnd=(HWND)cui_app_native_window(w->main_app);
    wchar_t title[128];
    MultiByteToWideChar(CP_UTF8,0,orca_label(L_ADVANCED),-1,title,128);
    ModifyMenuW(GetSystemMenu(hwnd,FALSE),ORCA_ADVANCED,MF_BYCOMMAND|MF_STRING,ORCA_ADVANCED,title);
}
static void browse_directory(window_state *w,char *path,size_t cap) {
    BROWSEINFOW info={0};info.hwndOwner=(HWND)cui_app_native_window(w->main_app);
    info.ulFlags=BIF_RETURNONLYFSDIRS;
    PIDLIST_ABSOLUTE id=SHBrowseForFolderW(&info);
    if(!id)return;
    wchar_t dir[MAX_PATH];
    if(SHGetPathFromIDListW(id,dir))WideCharToMultiByte(CP_UTF8,0,dir,-1,path,(int)cap,NULL,NULL);
    CoTaskMemFree(id);
}
static void frame(cui_ctx *ui, void *user) {
    const char *modes[] = { orca_label(L_FIXED), orca_label(L_PERCENT) };
    const char *languages[] = { orca_label(L_ENGLISH), orca_label(L_CHINESE) };
    static const char *codecs[] = { "H264", "HEVC", "AV1" };
    static const char *resolutions[] = { "3840 px", "2560 px", "1920 px", "1280 px" };
    window_state *window = (window_state *)user;
    if (!window->icon_tried) {
        window->icon_tried = 1;
        apply_window_icon((HWND)cui_app_native_window(window->main_app));
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
        cui_row(ui,.width=CUI_FILL,.gap=12) {
            cui_column(ui,.width=CUI_FILL) { cui_heading(ui,"dotOrcA",1); }
            if (cui_select(ui,"language",languages,2,&window->language,.label=orca_label(L_LANG),.width=CUI_PX(156))) {
                orca_set_language(window->language);
                if(window->old_proc)update_advanced_menu(window);
                cui_request_redraw(ui);
            }
        }
        cui_row(ui, .width = CUI_FILL, .gap = 12) {
            cui_select(ui, "target-mode", modes, 2, &window->target_mode,
                       .label = orca_label(L_CAP_MODE), .width = CUI_FILL);
            if (window->target_mode == 0) {
                cui_input(ui, "mib", window->mib, sizeof(window->mib),
                          .label = orca_label(L_CAP_FILE), .suffix = "MiB", .width = CUI_FILL);
            } else {
                cui_input(ui, "percent", window->percent, sizeof(window->percent),
                          .label = orca_label(L_OUTPUT_SOURCE), .suffix = "%", .width = CUI_FILL);
            }
            cui_select(ui, "codec", codecs, 3, &window->codec,
                       .label = orca_label(L_CODEC), .width = CUI_FILL);
            cui_select(ui, "max-edge", resolutions, 4, &window->resolution,
                       .label = orca_label(L_MAX_EDGE), .width = CUI_FILL);
        }
        cui_secondary(ui, window->target_mode == 0
            ? orca_label(L_FIXED_HELP)
            : orca_label(L_PERCENT_HELP));
        cui_column(ui, .width = CUI_FILL, .pad = 10, .bg = CUI_COLOR_SURFACE_RAISED) {
            cui_text(ui, orca_label(L_DROP));
        }
        if (window->font_tried && !window->font_ok) {
            cui_secondary(ui, orca_label(L_FONT_ERROR));
        }
        cui_scroll(ui, .id = "queue", .width = CUI_FILL, .height = CUI_FILL, .gap = 8) {
            for (uint32_t i = 0; i < count; ++i) draw_job(ui, window, i);
        }
        cui_text(ui, busy < 0 || window->callback_error ? orca_label(L_QUEUE_ERROR) : status, .width = CUI_FILL);
        cui_row(ui, .width = CUI_FILL, .gap = 8) {
            if (cui_button(ui, orca_label(L_START), .variant = CUI_BUTTON_PRIMARY, .disabled = !count)) {
                const char *value = window->target_mode == 0 ? window->mib : window->percent;
                window->callback_error = orca_start(value, strlen(value), window->target_mode,
                    window->codec, edges[window->resolution]) < 0;
                cui_request_redraw(ui);
            }
            if (cui_button(ui, orca_label(L_CANCEL_ALL), .disabled = !count)) action(ui, window, CANCEL, 0);
            if (cui_button(ui, orca_label(L_CLEAR_FINISHED), .disabled = !count)) {
                action(ui, window, CLEAR_FINISHED, 0);
            }
            if (cui_button(ui, orca_label(L_CLEAR_ALL), .disabled = !count)) {
                window->confirm_clear_all = true;
                cui_request_redraw(ui);
            }
        }
    }
    if (window->confirm_clear_all) {
        cui_dialog(ui, orca_label(L_CLEAR_ALL), &window->confirm_clear_all,
                   .width = 440, .description = orca_label(L_CLEAR_CONFIRM)) {
            cui_text(ui, orca_label(L_CLEAR_SAFE), .width = CUI_FILL);
            cui_dialog_footer(ui) {
                if (cui_button(ui, orca_label(L_KEEP))) {
                    window->confirm_clear_all = false;
                    cui_request_redraw(ui);
                }
                if (cui_button(ui, orca_label(L_CLEAR_ALL), .variant = CUI_BUTTON_PRIMARY)) {
                    window->confirm_clear_all = false;
                    close_detail(window);
                    action(ui, window, CLEAR_ALL, 0);
                }
            }
        }
    }
    if(window->advanced) {
        cui_dialog(ui,orca_label(L_ADVANCED),&window->advanced,.width=620) {
            cui_text(ui,orca_label(L_OUTPUT_DIR));
            cui_row(ui,.width=CUI_FILL,.gap=8) {
                cui_input(ui,"output-dir",window->output_dir,sizeof(window->output_dir),.width=CUI_FILL,.placeholder=orca_label(L_OUTPUT_HINT));
                if(cui_button(ui,orca_label(L_BROWSE)))browse_directory(window,window->output_dir,sizeof(window->output_dir));
            }
            cui_text(ui,orca_label(L_TOOLS_DIR));
            cui_row(ui,.width=CUI_FILL,.gap=8) {
                cui_input(ui,"tools-dir",window->tools_dir,sizeof(window->tools_dir),.width=CUI_FILL,.placeholder=orca_label(L_TOOLS_HINT));
                cui_push_id_int(ui,1);
                if(cui_button(ui,orca_label(L_BROWSE)))browse_directory(window,window->tools_dir,sizeof(window->tools_dir));
                cui_pop_id(ui);
            }
            if(window->advanced_error)cui_text(ui,orca_label(L_DIR_ERROR),.color=CUI_COLOR_DANGER,.width=CUI_FILL);
            cui_dialog_footer(ui) {
                if(cui_button(ui,orca_label(L_CANCEL)))window->advanced=false;
                if(cui_button(ui,orca_label(L_SAVE),.variant=CUI_BUTTON_PRIMARY)) {
                    if(orca_set_directories(window->output_dir,strlen(window->output_dir),window->tools_dir,strlen(window->tools_dir))==1)window->advanced=false;
                    else window->advanced_error=true;
                }
            }
        }
    }
    if (busy > 0 || window->detail) cui_request_wakeup(ui, 0.1f);
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
    window.language=orca_ui_init(PRIMARYLANGID(GetUserDefaultUILanguage()) == LANG_CHINESE) == 1;
    orca_settings saved_settings;
    if(orca_load_settings(&saved_settings)==1){
        memcpy(window.mib,saved_settings.mib,sizeof(window.mib));
        memcpy(window.percent,saved_settings.percent,sizeof(window.percent));
        window.target_mode=saved_settings.mode;window.codec=saved_settings.codec;window.resolution=saved_settings.resolution;
    }
    orca_placement saved;
    cui_window_placement placement={0};
    if(orca_load_placement(0,&saved)==1){
        placement.x=saved.x;placement.y=saved.y;placement.width=saved.width;placement.height=saved.height;
        placement.state=saved.state?CUI_WINDOW_MAXIMIZED:CUI_WINDOW_NORMAL;placement.has_position=saved.has_position!=0;
        desc.placement=&placement;
    }
    window.main_app=cui_app_create(&desc);
    if (!window.main_app) return 1;
    install_advanced(&window);
    while (cui_app_frame(window.main_app)) {
        frame(cui_app_ui(window.main_app),&window);
        tick_detail(&window);
    }
    close_detail(&window);
    cui_window_placement pos;
    if(cui_app_placement(window.main_app,&pos)){
        orca_placement v={pos.x,pos.y,pos.width,pos.height,pos.state==CUI_WINDOW_MAXIMIZED,pos.has_position};
        orca_save_placement(0,&v);
    }
    orca_settings v={0};
    memcpy(v.mib,window.mib,sizeof(v.mib));memcpy(v.percent,window.percent,sizeof(v.percent));
    v.mode=window.target_mode;v.codec=window.codec;v.resolution=window.resolution;
    orca_save_settings(&v);
    HWND hwnd=(HWND)cui_app_native_window(window.main_app);
    if(window.old_proc)SetWindowLongPtrW(hwnd,GWLP_WNDPROC,(LONG_PTR)window.old_proc);
    RemovePropW(hwnd,L"dotOrcA.window");
    cui_app_destroy(window.main_app);
    return 0;
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
    window.main_app=app;
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
    result = 17;
    if (!open_detail(&window,a.id)) goto done;
    tick_detail(&window);
    if (!window.detail || !cui_app_is_software(window.detail->app)) goto done;
    close_detail(&window);
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
    close_detail(&window);
    cui_app_destroy(app);
    return result;
}
