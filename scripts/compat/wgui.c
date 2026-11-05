/* A Windows program with a window, for scripts/test-compat.sh: a window class,
 * the message loop, posted messages, a timer, GDI drawing, and reading a
 * pixel back. It closes itself after three timer ticks.
 * Build: x86_64-w64-mingw32-gcc -O2 -mwindows -o wgui.exe wgui.c */
#include <stdio.h>
#include <windows.h>

static int ticks, paints;

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM wp, LPARAM lp) {
    switch (m) {
    case WM_CREATE:
        printf("wgui: create\n");
        SetTimer(h, 1, 150, NULL);
        PostMessageA(h, WM_USER + 1, 42, 0);
        return 0;
    case WM_USER + 1:
        printf("wgui: user message %d\n", (int)wp);
        return 0;
    case WM_PAINT: {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(h, &ps);
        RECT r = {20, 20, 120, 80};
        HBRUSH red = CreateSolidBrush(RGB(255, 0, 0));
        FillRect(dc, &r, red);
        DeleteObject(red);
        HBRUSH green = CreateSolidBrush(RGB(0, 160, 0));
        HGDIOBJ old = SelectObject(dc, green);
        SelectObject(dc, GetStockObject(BLACK_PEN));
        Ellipse(dc, 140, 20, 240, 100);
        SelectObject(dc, old);
        DeleteObject(green);
        MoveToEx(dc, 20, 120, NULL);
        LineTo(dc, 300, 120);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, RGB(0, 0, 128));
        TextOutA(dc, 20, 140, "Ciao da Win32!", 14);
        RECT tr = {0, 180, 320, 220};
        DrawTextA(dc, "centrato", -1, &tr, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
        EndPaint(h, &ps);
        paints++;
        return 0;
    }
    case WM_TIMER:
        if (++ticks == 3) {
            HDC dc = GetDC(h);
            COLORREF c = GetPixel(dc, 50, 50);
            ReleaseDC(h, dc);
            printf("wgui: painted %d pixel %06lx\n", paints > 0, (unsigned long)c);
            KillTimer(h, 1);
            DestroyWindow(h);
        }
        return 0;
    case WM_DESTROY:
        PostQuitMessage(7);
        return 0;
    }
    return DefWindowProcA(h, m, wp, lp);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show) {
    (void)prev;
    (void)cmd;
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = proc;
    wc.hInstance = inst;
    wc.lpszClassName = "justwin";
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    RegisterClassA(&wc);
    RECT r = {0, 0, 320, 240};
    AdjustWindowRect(&r, WS_OVERLAPPEDWINDOW, FALSE);
    HWND h = CreateWindowExA(0, "justwin", "Win32 su JustOS", WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT,
                             r.right - r.left, r.bottom - r.top, NULL, NULL, inst, NULL);
    if (!h) {
        printf("wgui: no window\n");
        return 1;
    }
    ShowWindow(h, show);
    UpdateWindow(h);
    MSG msg;
    while (GetMessageA(&msg, NULL, 0, 0) > 0) {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    printf("wgui: quit %d\n", (int)msg.wParam);
    return 0;
}
