// A Wayland client for JustOS's compositor, against Debian's
// libwayland-client and libxkbcommon (as GTK and Firefox use them). It opens
// a window of 300x200 (green, with a magenta subsurface at 20,20), waits
// for its frame callback and the buffer's release, reads the keymap with
// xkbcommon, reports pointer and keyboard input, opens a popup on a right
// click, draws itself again at the size the desktop asks (full screen) and
// closes when the desktop says so. Each line starts with "wl:";
// scripts/test-wayland.sh drives it and checks them.
//
// Build: gcc wl-test.c xdg-shell-protocol.c xdg-decoration-protocol.c
//        -lwayland-client -lxkbcommon   (the protocol files come from
//        wayland-scanner)
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include "xdg-shell-client-protocol.h"
#include "xdg-decoration-unstable-v1-client-protocol.h"

#define W 300
#define H 200
#define GREEN 0xff20b060u
#define MAGENTA 0xffc02080u

static struct wl_compositor *compositor;
static struct wl_subcompositor *subcompositor;
static struct wl_shm *shm;
static struct wl_seat *seat;
static struct wl_output *output;
static struct xdg_wm_base *wm;
static struct zxdg_decoration_manager_v1 *decorations;
static int have_kde, have_data;
static int argb, xrgb;
static int out_w, out_h, out_scale, out_done;

static struct wl_surface *surface, *sub_surface, *popup_surface;
static struct xdg_surface *xsurface, *popup_xsurface;
static struct xdg_toplevel *toplevel;
static struct xdg_popup *popup;
static int configured, conf_w = -1, conf_h = -1, activated, cur_w = W, cur_h = H;
static int decoration_mode;
static int frames, released, closing;

static struct xkb_context *xkb;
static struct xkb_keymap *keymap;
static struct xkb_state *xstate;
static char typed[256];
static int ntyped;
static uint32_t last_serial;

static void noop() {}

static struct wl_buffer *make_buffer(int w, int h, uint32_t colour, int format) {
    int stride = w * 4, size = stride * h;
    int fd = memfd_create("wl-test", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, size) < 0) {
        perror("wl: memfd");
        exit(1);
    }
    uint32_t *px = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (px == MAP_FAILED) {
        perror("wl: mmap");
        exit(1);
    }
    for (int i = 0; i < w * h; i++)
        px[i] = format == WL_SHM_FORMAT_XRGB8888 ? (colour & 0xffffff) : colour;
    munmap(px, size);
    struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, size);
    struct wl_buffer *b = wl_shm_pool_create_buffer(pool, 0, w, h, stride, format);
    wl_shm_pool_destroy(pool);
    close(fd);
    return b;
}

// ---------------------------------------------------------------------------
// Globals

static void shm_format(void *d, struct wl_shm *s, uint32_t f) {
    if (f == WL_SHM_FORMAT_ARGB8888) argb = 1;
    if (f == WL_SHM_FORMAT_XRGB8888) xrgb = 1;
}
static const struct wl_shm_listener shm_listener = {shm_format};

static void out_mode(void *d, struct wl_output *o, uint32_t flags, int32_t w, int32_t h, int32_t r) {
    if (flags & WL_OUTPUT_MODE_CURRENT) out_w = w, out_h = h;
}
static void out_scale_ev(void *d, struct wl_output *o, int32_t s) { out_scale = s; }
static void out_done_ev(void *d, struct wl_output *o) { out_done = 1; }
static void out_geometry(void *d, struct wl_output *o, int32_t x, int32_t y, int32_t pw, int32_t ph, int32_t sp,
                         const char *make, const char *model, int32_t t) {}
static void out_name(void *d, struct wl_output *o, const char *n) {}
static const struct wl_output_listener output_listener = {out_geometry, out_mode, out_done_ev, out_scale_ev, out_name, out_name};

static void wm_ping(void *d, struct xdg_wm_base *w, uint32_t serial) { xdg_wm_base_pong(w, serial); }
static const struct xdg_wm_base_listener wm_listener = {wm_ping};

static void global(void *d, struct wl_registry *r, uint32_t name, const char *iface, uint32_t v) {
#define MIN(a, b) ((a) < (b) ? (a) : (b))
    if (!strcmp(iface, "wl_compositor")) compositor = wl_registry_bind(r, name, &wl_compositor_interface, MIN(v, 5));
    else if (!strcmp(iface, "wl_subcompositor")) subcompositor = wl_registry_bind(r, name, &wl_subcompositor_interface, 1);
    else if (!strcmp(iface, "wl_shm")) {
        shm = wl_registry_bind(r, name, &wl_shm_interface, 1);
        wl_shm_add_listener(shm, &shm_listener, NULL);
    } else if (!strcmp(iface, "wl_seat")) seat = wl_registry_bind(r, name, &wl_seat_interface, MIN(v, 7));
    else if (!strcmp(iface, "wl_output")) {
        output = wl_registry_bind(r, name, &wl_output_interface, MIN(v, 4));
        wl_output_add_listener(output, &output_listener, NULL);
    } else if (!strcmp(iface, "xdg_wm_base")) {
        wm = wl_registry_bind(r, name, &xdg_wm_base_interface, MIN(v, 5));
        xdg_wm_base_add_listener(wm, &wm_listener, NULL);
    } else if (!strcmp(iface, "zxdg_decoration_manager_v1"))
        decorations = wl_registry_bind(r, name, &zxdg_decoration_manager_v1_interface, 1);
    else if (!strcmp(iface, "org_kde_kwin_server_decoration_manager")) have_kde = 1;
    else if (!strcmp(iface, "wl_data_device_manager")) have_data = 1;
}
static const struct wl_registry_listener registry_listener = {global, noop};

// ---------------------------------------------------------------------------
// The window

static void xsurface_configure(void *d, struct xdg_surface *s, uint32_t serial) {
    xdg_surface_ack_configure(s, serial);
    configured++;
}
static const struct xdg_surface_listener xsurface_listener = {xsurface_configure};

static void top_configure(void *d, struct xdg_toplevel *t, int32_t w, int32_t h, struct wl_array *states) {
    conf_w = w, conf_h = h;
    activated = 0;
    uint32_t *st;
    wl_array_for_each(st, states) if (*st == XDG_TOPLEVEL_STATE_ACTIVATED) activated = 1;
}
static void top_close(void *d, struct xdg_toplevel *t) {
    printf("wl: closed by the desktop\n");
    closing = 1;
}
static void top_bounds(void *d, struct xdg_toplevel *t, int32_t w, int32_t h) {
    printf("wl: bounds %dx%d\n", w, h);
}
static void top_caps(void *d, struct xdg_toplevel *t, struct wl_array *caps) {}
static const struct xdg_toplevel_listener top_listener = {top_configure, top_close, top_bounds, top_caps};

static void deco_configure(void *d, struct zxdg_toplevel_decoration_v1 *z, uint32_t mode) { decoration_mode = mode; }
static const struct zxdg_toplevel_decoration_v1_listener deco_listener = {deco_configure};

static void frame_done(void *d, struct wl_callback *cb, uint32_t t) {
    frames++;
    wl_callback_destroy(cb);
}
static const struct wl_callback_listener frame_listener = {frame_done};

static void buffer_release(void *d, struct wl_buffer *b) {
    released++;
    wl_buffer_destroy(b);
}
static const struct wl_buffer_listener buffer_listener = {buffer_release};

// A popup at the pointer, the way a context menu opens.
static void popup_configure(void *d, struct xdg_popup *p, int32_t x, int32_t y, int32_t w, int32_t h) {
    printf("wl: popup at %d,%d %dx%d\n", x, y, w, h);
}
static void popup_done(void *d, struct xdg_popup *p) {
    printf("wl: popup done\n");
    xdg_popup_destroy(popup);
    xdg_surface_destroy(popup_xsurface);
    wl_surface_destroy(popup_surface);
    popup = NULL;
}
static void popup_repositioned(void *d, struct xdg_popup *p, uint32_t token) {}
static const struct xdg_popup_listener popup_listener = {popup_configure, popup_done, popup_repositioned};

static void popup_xconfigure(void *d, struct xdg_surface *s, uint32_t serial) {
    xdg_surface_ack_configure(s, serial);
    struct wl_buffer *b = make_buffer(80, 50, MAGENTA, WL_SHM_FORMAT_ARGB8888);
    wl_buffer_add_listener(b, &buffer_listener, NULL);
    wl_surface_attach(popup_surface, b, 0, 0);
    wl_surface_damage_buffer(popup_surface, 0, 0, 80, 50);
    wl_surface_commit(popup_surface);
}
static const struct xdg_surface_listener popup_xlistener = {popup_xconfigure};

static void open_popup(int x, int y) {
    struct xdg_positioner *p = xdg_wm_base_create_positioner(wm);
    xdg_positioner_set_size(p, 80, 50);
    xdg_positioner_set_anchor_rect(p, x, y, 1, 1);
    xdg_positioner_set_anchor(p, XDG_POSITIONER_ANCHOR_BOTTOM_RIGHT);
    xdg_positioner_set_gravity(p, XDG_POSITIONER_GRAVITY_BOTTOM_RIGHT);
    xdg_positioner_set_constraint_adjustment(p, XDG_POSITIONER_CONSTRAINT_ADJUSTMENT_FLIP_X |
                                                    XDG_POSITIONER_CONSTRAINT_ADJUSTMENT_FLIP_Y |
                                                    XDG_POSITIONER_CONSTRAINT_ADJUSTMENT_SLIDE_X |
                                                    XDG_POSITIONER_CONSTRAINT_ADJUSTMENT_SLIDE_Y);
    popup_surface = wl_compositor_create_surface(compositor);
    popup_xsurface = xdg_wm_base_get_xdg_surface(wm, popup_surface);
    xdg_surface_add_listener(popup_xsurface, &popup_xlistener, NULL);
    popup = xdg_surface_get_popup(popup_xsurface, xsurface, p);
    xdg_popup_add_listener(popup, &popup_listener, NULL);
    xdg_popup_grab(popup, seat, last_serial);
    xdg_positioner_destroy(p);
    wl_surface_commit(popup_surface);
}

// ---------------------------------------------------------------------------
// Input

static void p_enter(void *d, struct wl_pointer *p, uint32_t serial, struct wl_surface *s, wl_fixed_t x, wl_fixed_t y) {
    static int once;
    if (!once++) printf("wl: pointer enter %s\n", s == surface ? "window" : s == sub_surface ? "subsurface" : "other");
}
static void p_leave(void *d, struct wl_pointer *p, uint32_t serial, struct wl_surface *s) {}
static void p_motion(void *d, struct wl_pointer *p, uint32_t t, wl_fixed_t x, wl_fixed_t y) {}
static void p_button(void *d, struct wl_pointer *p, uint32_t serial, uint32_t t, uint32_t button, uint32_t state) {
    last_serial = serial;
    if (state != WL_POINTER_BUTTON_STATE_PRESSED) return;
    printf("wl: button %#x\n", button);
    if (button == 0x111 && !popup) open_popup(200, 150);
}
static void p_axis(void *d, struct wl_pointer *p, uint32_t t, uint32_t axis, wl_fixed_t v) {
    printf("wl: scroll %d\n", wl_fixed_to_int(v));
}
static const struct wl_pointer_listener pointer_listener = {p_enter, p_leave, p_motion, p_button, p_axis, noop, noop, noop, noop};

static void k_keymap(void *d, struct wl_keyboard *k, uint32_t format, int32_t fd, uint32_t size) {
    char *text = mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0);
    if (text == MAP_FAILED) {
        perror("wl: keymap mmap");
        close(fd);
        return;
    }
    struct xkb_keymap *km = xkb_keymap_new_from_string(xkb, text, XKB_KEYMAP_FORMAT_TEXT_V1, 0);
    munmap(text, size);
    close(fd);
    if (!km) {
        printf("wl: keymap rejected\n");
        return;
    }
    static int once;
    if (!once++) printf("wl: keymap ok format=%u keys=%u\n", format, xkb_keymap_max_keycode(km) - xkb_keymap_min_keycode(km) + 1);
    if (keymap) xkb_keymap_unref(keymap), xkb_state_unref(xstate);
    keymap = km;
    xstate = xkb_state_new(km);
}
static void k_enter(void *d, struct wl_keyboard *k, uint32_t serial, struct wl_surface *s, struct wl_array *keys) {
    static int once;
    if (!once++) printf("wl: keyboard enter\n");
}
static void k_leave(void *d, struct wl_keyboard *k, uint32_t serial, struct wl_surface *s) {}
static void k_key(void *d, struct wl_keyboard *k, uint32_t serial, uint32_t t, uint32_t key, uint32_t state) {
    if (state != WL_KEYBOARD_KEY_STATE_PRESSED || !xstate) return;
    xkb_keysym_t sym = xkb_state_key_get_one_sym(xstate, key + 8);
    if (sym == XKB_KEY_Return) {
        typed[ntyped] = 0;
        printf("wl: typed %s\n", typed);
        ntyped = 0;
        return;
    }
    if (xkb_state_mod_name_is_active(xstate, XKB_MOD_NAME_CTRL, XKB_STATE_MODS_EFFECTIVE) > 0) {
        printf("wl: ctrl+%c\n", (char)xkb_keysym_to_utf32(sym));
        return;
    }
    char buf[8];
    int n = xkb_state_key_get_utf8(xstate, key + 8, buf, sizeof buf);
    if (n > 0 && ntyped + n < (int)sizeof typed - 1) memcpy(typed + ntyped, buf, n), ntyped += n;
}
static void k_mods(void *d, struct wl_keyboard *k, uint32_t serial, uint32_t dep, uint32_t lat, uint32_t lock, uint32_t group) {
    if (xstate) xkb_state_update_mask(xstate, dep, lat, lock, 0, 0, group);
}
static void k_repeat(void *d, struct wl_keyboard *k, int32_t rate, int32_t delay) {}
static const struct wl_keyboard_listener keyboard_listener = {k_keymap, k_enter, k_leave, k_key, k_mods, k_repeat};

// ---------------------------------------------------------------------------

int main(void) {
    struct wl_display *dpy = wl_display_connect(NULL);
    if (!dpy) {
        printf("wl: cannot connect (WAYLAND_DISPLAY=%s)\n", getenv("WAYLAND_DISPLAY"));
        return 1;
    }
    printf("wl: connected\n");
    xkb = xkb_context_new(XKB_CONTEXT_NO_DEFAULT_INCLUDES | XKB_CONTEXT_NO_ENVIRONMENT_NAMES);
    struct wl_registry *reg = wl_display_get_registry(dpy);
    wl_registry_add_listener(reg, &registry_listener, NULL);
    wl_display_roundtrip(dpy);
    wl_display_roundtrip(dpy);
    printf("wl: globals compositor=%d sub=%d shm=%d seat=%d output=%d wm=%d deco=%d kde=%d data=%d\n", !!compositor,
           !!subcompositor, !!shm, !!seat, !!output, !!wm, !!decorations, have_kde, have_data);
    printf("wl: shm argb=%d xrgb=%d\n", argb, xrgb);
    printf("wl: output %dx%d scale=%d done=%d\n", out_w, out_h, out_scale, out_done);
    if (!compositor || !shm || !wm || !seat) return 1;

    wl_pointer_add_listener(wl_seat_get_pointer(seat), &pointer_listener, NULL);
    wl_keyboard_add_listener(wl_seat_get_keyboard(seat), &keyboard_listener, NULL);

    surface = wl_compositor_create_surface(compositor);
    xsurface = xdg_wm_base_get_xdg_surface(wm, surface);
    xdg_surface_add_listener(xsurface, &xsurface_listener, NULL);
    toplevel = xdg_surface_get_toplevel(xsurface);
    xdg_toplevel_add_listener(toplevel, &top_listener, NULL);
    xdg_toplevel_set_title(toplevel, "Prova Wayland");
    xdg_toplevel_set_app_id(toplevel, "wl-test");
    if (decorations) {
        struct zxdg_toplevel_decoration_v1 *z = zxdg_decoration_manager_v1_get_toplevel_decoration(decorations, toplevel);
        zxdg_toplevel_decoration_v1_add_listener(z, &deco_listener, NULL);
        zxdg_toplevel_decoration_v1_set_mode(z, ZXDG_TOPLEVEL_DECORATION_V1_MODE_CLIENT_SIDE);
    }
    wl_surface_commit(surface);
    while (!configured && wl_display_dispatch(dpy) >= 0) {}
    printf("wl: configured %dx%d decoration=%d\n", conf_w, conf_h, decoration_mode);

    // A subsurface, drawn first: it shows when its parent does.
    sub_surface = wl_compositor_create_surface(compositor);
    struct wl_subsurface *sub = wl_subcompositor_get_subsurface(subcompositor, sub_surface, surface);
    wl_subsurface_set_position(sub, 20, 20);
    wl_subsurface_set_desync(sub);
    struct wl_buffer *sb = make_buffer(60, 40, MAGENTA, WL_SHM_FORMAT_XRGB8888);
    wl_buffer_add_listener(sb, &buffer_listener, NULL);
    wl_surface_attach(sub_surface, sb, 0, 0);
    wl_surface_damage(sub_surface, 0, 0, 60, 40);
    wl_surface_commit(sub_surface);

    struct wl_buffer *b = make_buffer(W, H, GREEN, WL_SHM_FORMAT_ARGB8888);
    wl_buffer_add_listener(b, &buffer_listener, NULL);
    wl_surface_attach(surface, b, 0, 0);
    wl_surface_damage_buffer(surface, 0, 0, W, H);
    wl_callback_add_listener(wl_surface_frame(surface), &frame_listener, NULL);
    wl_surface_commit(surface);
    while ((!frames || released < 2) && wl_display_dispatch(dpy) >= 0) {}
    printf("wl: frame done %d released %d activated=%d\n", frames, released, activated);
    fflush(stdout);

    while (!closing && wl_display_dispatch(dpy) >= 0) {
        // The desktop asked for another size: draw at it.
        if (conf_w > 0 && conf_h > 0 && (conf_w != cur_w || conf_h != cur_h)) {
            cur_w = conf_w, cur_h = conf_h;
            struct wl_buffer *nb = make_buffer(cur_w, cur_h, GREEN, WL_SHM_FORMAT_ARGB8888);
            wl_buffer_add_listener(nb, &buffer_listener, NULL);
            wl_surface_attach(surface, nb, 0, 0);
            wl_surface_damage_buffer(surface, 0, 0, cur_w, cur_h);
            wl_surface_commit(surface);
            printf("wl: resized to %dx%d\n", cur_w, cur_h);
        }
        fflush(stdout);
    }
    xdg_toplevel_destroy(toplevel);
    xdg_surface_destroy(xsurface);
    wl_surface_destroy(surface);
    wl_display_roundtrip(dpy);
    wl_display_disconnect(dpy);
    printf("wl: bye\n");
    return 0;
}
