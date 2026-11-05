# Programmi del negozio (sys:/store)

Ogni programma è in una cartella: `app` (il programma), `info` (la scheda
del Negozio) e, se c'è, `icon.qoi`.

Le app con finestra (2048, Campo minato, Serpente, Sudoku, Tris, Disegno) sono
programmi JustOS di questo repository, in `system/store/`.

BusyBox e Lua sono programmi liberi compilati dai sorgenti ufficiali, collegati staticamente con
[musl](https://musl.libc.org/) per x86-64 Linux. JustOS li esegue con il suo
livello di compatibilità Linux (`kernel/src/linux.rs`). Nessuna modifica ai
sorgenti.

| Programma | Sorgente | Versione | Licenza | Comando |
|---|---|---|---|---|
| `busybox/app` | https://github.com/mirror/busybox | commit 371fe9f | GPL-2.0 | `musl-gcc`, `allnoconfig` + applet scelti, `CONFIG_STATIC=y` |
| `lua/app`     | https://github.com/lua/lua         | 5.4 (8cb93ba) | MIT | `make CC=musl-gcc MYLDFLAGS=-static` |

Per ricompilarli servono `musl-gcc` (pacchetto `musl-tools`) e `make`.
