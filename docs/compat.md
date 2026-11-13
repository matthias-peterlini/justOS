# Programmi Linux, Windows e Android su justOS

justOS esegue, oltre ai suoi programmi nativi, anche
programmi **Linux** e **Windows** per x86-64 e app **Android** e programmi
Java (vedi [più sotto](#programmi-java-e-app-android)), senza modificarli. Non c'è una
macchina virtuale: il programma gira nello stesso modo degli altri, in ring 3
e nel suo spazio di indirizzi; quando chiede qualcosa al sistema, justOS traduce
la richiesta nelle proprie chiamate.

L'idea è la stessa di [Wine](https://www.winehq.org/) e di
[WSL](https://learn.microsoft.com/windows/wsl/), in piccolo: nessun codice di
Microsoft o di Linux è copiato in justOS. Il supporto è scritto da zero a
partire dalle interfacce pubbliche (la ABI di System V per Linux, la
convenzione x64 e i pochi `.dll` di sistema per Windows).

## Programmi Linux

justOS carica un eseguibile ELF x86-64 e serve le chiamate di sistema Linux
che fa. Il programma può essere **collegato staticamente** (con
[musl](https://musl.libc.org/), `musl-gcc -static`, o costruito da Go e Rust)
oppure **dinamicamente con la glibc**, come quasi tutti i programmi delle
distribuzioni. Per questi il kernel carica il caricatore dinamico che il
programma chiede (`/lib64/ld-linux-x86-64.so.2`) e parte da lì; il
caricatore trova il programma nel vettore ausiliario e mappa le librerie
condivise con `mmap`.

Il caricatore, la glibc e le altre librerie vengono dai **pacchetti di
Debian**, non sono ricompilati: `tools/linux/debroot.py` scarica i pacchetti
indicati (controllando le somme SHA-256 dell'archivio), segue le loro
dipendenze e li scompatta in `build/winelinux` (con Wine) o in `build/linux`
(senza, `make WINE=0` e i test). L'immagine li porta nel volume di
sistema con la disposizione di Debian: tutto in `/usr`, con `/bin`, `/sbin`,
`/lib` e `/lib64` come collegamenti simbolici dentro `/usr` (JustFS ha i
collegamenti simbolici). L'app File non mostra queste cartelle. Quali
pacchetti, lo dice `LINUX_PKGS` nel `Makefile` (per ora `libc6`, la glibc
2.41 di Debian 13 con la sua localizzazione C.UTF-8, `libwayland-client0`
con `libxkbcommon0` per i programmi con finestre, GTK 3 e Firefox), più
`WINE_PKGS` (Wine) se non c'è `WINE=0`; `make linux-root` rifà l'albero. Il
CD live non le porta: il suo volume di
sistema viene letto in memoria all'avvio, e l'albero di Debian è più
grande di tutto il resto.

Un programma Linux si avvia come gli altri, per percorso o per nome:

```
sys:/home/hello            un eseguibile Linux qualunque
busybox ls -la             un programma installato (vedi "Installare programmi")
```

### Cosa vede il programma

| Area | Su justOS |
|---|---|
| `stdin`, `stdout`, `stderr` | la console; `stdin` legge una riga per volta, con l'eco dei tasti |
| file e cartelle | i volumi montati: il "/" di Linux è la radice del volume corrente, così `/home/x` è `<volume>:/home/x` |
| memoria | `brk` e `mmap` anonimo (il programma alloca e libera normalmente) |
| memoria condivisa | `memfd_create` (con i sigilli di `F_ADD_SEALS`), `shm_open` in `/dev/shm`, `MAP_SHARED` anonimo: chi la mappa vede le stesse pagine, anche tra processi diversi dopo `fork` o passandosi il descrittore su un socket; anche un `MAP_SHARED` di un file: le stesse pagine per tutti i processi, che `read` e `write` vedono e che tornano nel file (con `msync`, `fsync` e quando l'ultima mappatura se ne va) |
| orologio | `clock_gettime`, `gettimeofday`, `nanosleep`; `timerfd` (una volta o periodico, anche a un'ora assoluta), che `poll` ed `epoll` aspettano |
| TLS | `arch_prctl(ARCH_SET_FS)` imposta la base del segmento FS del thread |
| casualità | `getrandom` |
| librerie condivise | il caricatore dinamico della glibc, `mmap` dei file (i byte di un file mappato più volte sono condivisi), `dlopen` |
| thread | `clone` per i thread, `futex`, `set_tid_address` |
| rete | socket TCP e UDP, `poll`, `select`, `epoll` |
| socket locali | `AF_UNIX` di tipo stream, datagram e seqpacket: `socketpair`, `bind`, `listen`, `connect`, `accept`, nomi su un percorso o astratti, descrittori passati con `SCM_RIGHTS`, `SO_PEERCRED`; il nome su un percorso non è un file del volume, ma `stat`, `access` e `unlink` lo vedono |
| collegamenti simbolici | `symlink`, `readlink`, `lstat` |
| finestre | Wayland: `WAYLAND_DISPLAY=wayland-0` in `XDG_RUNTIME_DIR=/home/tmp`, servito dal kernel (vedi sotto) |
| utenti | tutto gira come `root` |
| processi nuovi | `fork` (il figlio ha una copia della memoria), `vfork` e `posix_spawn` (il figlio usa la memoria del padre finché non chiama `execve`), `execve` (anche degli script che iniziano con `#!`), `wait4`, `waitid`, `getppid`; i descrittori aperti passano al figlio, quelli con `O_CLOEXEC` si chiudono con `execve` |
| segnali | gestori con `sigaction` (`SA_SIGINFO`, `SA_RESTART`, `SA_ONSTACK`, `SA_RESETHAND`, `SA_NODEFER`) e `sigaltstack`, maschere per thread, `kill`, `tgkill`, `sigpending`, `sigtimedwait`, `sigsuspend`, `pause`, `SIGCHLD` quando un figlio finisce; `signalfd`, che legge i segnali bloccati (anche `SIGCHLD` con lo stato del figlio) come fanno l'init di Android e systemd; un'attesa interrotta da un gestore risponde `EINTR` o riparte da sola con `SA_RESTART`; un accesso non valido diventa `SIGSEGV` (e `SIGBUS`, `SIGILL`, `SIGFPE`) per chi ha un gestore; Ctrl+C manda `SIGINT` ai programmi che lo gestiscono; scrivere su una pipe o un socket che nessuno legge manda `SIGPIPE`; niente controllo dei job (`SIGSTOP`, `SIGTSTP`) |

Le chiamate implementate bastano ai programmi da riga di comando: lettura e
scrittura di file, elenco di cartelle (`getdents64`), `stat`, `ioctl` per sapere
se l'uscita è un terminale, e così via. Una chiamata non ancora prevista compare
nel giornale di sistema come `unimplemented syscall N` (con `verbose on`), così
è facile vedere cosa manca.

Com'è fatto: `system/kernel/src/linux/` (il caricamento e le chiamate) e
`system/kernel/src/arch/syscall.rs` (l'istruzione `syscall` e le basi FS/GS di
ogni thread).

### Programmi con finestre (Wayland)

I programmi Linux con un'interfaccia grafica (GTK, Qt, SDL, i browser)
parlano con un **compositor Wayland**. Su justOS il compositor è nel kernel
(`system/kernel/src/linux/wayland/`): ascolta su `/home/tmp/wayland-0`, lo
stesso posto da ogni volume, e serve ogni programma con un thread suo. Il
programma usa la sua `libwayland-client` di Debian, senza modifiche.

Ogni finestra del programma diventa una **finestra di justOS**, con la
cornice, i pulsanti, il vetro e le scorciatoie di tutte le altre (Ctrl+Q la
chiude, F11 la mette a schermo intero): le decorazioni sono del server
(`xdg-decoration`, e il protocollo di KDE, che GTK guarda), così i programmi
non disegnano la loro barra del titolo. Il contenuto è quello che il
programma disegna.

| Protocollo | Cosa c'è |
|---|---|
| `wl_compositor`, `wl_subcompositor` | superfici, regioni, sottosuperfici (con il loro ordine), callback di frame legati al disegno della scrivania |
| `wl_shm` | buffer in memoria condivisa (`memfd`), ARGB8888 e XRGB8888; il compositor copia solo la parte cambiata e libera subito il buffer |
| `wl_output` | lo schermo, con la sua scala: su uno schermo ad alta densità il programma disegna al doppio dei pixel |
| `xdg_wm_base` | finestre (`xdg_toplevel`: titolo, dimensioni minime e massime, ridimensionamento dalla cornice, stato attivo, chiusura) e popup (`xdg_popup`, posizionati con le regole del loro `xdg_positioner` dentro la finestra; un clic fuori chiude un menu) |
| `wp_viewporter` | una parte del buffer, mostrata a una dimensione data (Wine disegna in buffer più grandi delle sue finestre) |
| `wl_seat` | puntatore (movimento, pulsanti, rotellina) e tastiera, con una mappa XKB di justOS: i tasti arrivano già tradotti dal layout scelto, anche le lettere accentate |
| `wl_data_device_manager` | presente, ma gli appunti non passano ancora tra i programmi Linux e justOS |

`make test-wayland` lo prova con un piccolo client (`scripts/linux/wl-test.c`)
compilato al volo contro la `libwayland-client` e la `libxkbcommon` di Debian.

### GTK 3

L'immagine porta **GTK 3** di Debian (`libgtk-3-0t64`, con il caricatore SVG
delle icone e i programmi di prova `gtk3-widget-factory` e `gtk3-demo` in
`sys:/usr/bin`). I programmi GTK hanno l'aspetto di justOS: il tema
**justOS** (`rootfs/usr/share/themes/justOS/gtk-3.0/gtk.css`, scelto da
`rootfs/etc/gtk-3.0/settings.ini`) lascia trasparente lo sfondo, così dietro
si vede il vetro della finestra, e usa testo bianco, pulsanti a pillola,
l'arancione di justOS per spunte, interruttori e barre, e il carattere Google
Sans Flex. Parte da Adwaita scuro (che GTK ha dentro di sé) e copre anche lo
stato delle finestre che non sono davanti: una finestra GTK resta uguale
davanti e dietro, come quelle di justOS.

Non ci sono ancora inotify (GIO ripiega su controlli periodici), D-Bus e
l'accessibilità; le impostazioni (GSettings) restano in memoria. L'avvio di
un programma GTK prende qualche secondo.

`make test-gtk` avvia `gtk3-widget-factory` dalla console seriale e
controlla la sua finestra sullo schermo: c'è, ha i colori del tema e non il
blu di Adwaita.

### Firefox

**Firefox ESR** di Debian è nel sistema (`firefox-esr` in `LINUX_PKGS`;
per questo anche l'immagine senza Wine è di 1 GiB) e gira con i suoi processi figli e le
pagine web dalla rete. Sotto emulazione, senza accelerazione, la prima
finestra arriva dopo una quarantina di secondi.

I programmi Linux con una finestra compaiono tra le applicazioni (e nella
ricerca) da soli: il launcher legge i file `.desktop` di
`sys:/usr/share/applications` (nome, anche in italiano, programma, icona;
quelli con `NoDisplay` o `Terminal` restano fuori) e prende l'icona PNG dal
tema `hicolor`. La stessa icona va nel dock mentre il programma è aperto.

Le pagine web (`.html`) e i link si aprono in Firefox quando c'è. Se Firefox
è già aperto, una seconda pagina non gli si può ancora passare (manca D-Bus):
una notifica lo dice. Firefox vede solo i file del volume di sistema.

## Programmi Windows

I programmi Windows li esegue `winrun`, un programma di justOS
(`system/apps/winrun`). `winrun` carica l'eseguibile `.exe` (PE32+ per x64),
mette i propri indirizzi nella tabella delle importazioni al posto delle
funzioni di `kernel32.dll` e `msvcrt.dll`, prepara un blocco di thread (TEB) e
salta al punto d'ingresso. Il programma gira nello stesso processo di `winrun`:
quando chiama, per esempio, `printf`, chiama il codice di `winrun`, che a sua
volta usa le chiamate di justOS.

Un `.exe` si avvia per nome (justOS riconosce l'intestazione `MZ` e usa `winrun`
da solo) o esplicitamente:

```
hello.exe                  justOS vede che è un programma Windows e lo avvia
winrun hello.exe alpha     lo stesso, scritto per esteso
```

Sono gestiti i **programmi x64** costruiti con
[MinGW-w64](https://www.mingw-w64.org/) (`x86_64-w64-mingw32-gcc`), da console
e con finestre:

| Area | Su justOS |
|---|---|
| console | i tre flussi standard, `printf`/`scanf` e famiglia, `fgets(stdin)`, `getchar`, `ReadConsole` |
| file | `fopen`/`fread`/`fgets`/`fscanf`/`fseek` e `CreateFile`/`ReadFile`/`WriteFile`; i percorsi Windows (`C:\x`) diventano percorsi di justOS |
| runtime C | memoria (`malloc`, `HeapAlloc`, `VirtualAlloc`), stringhe, `ctype`, `strtol`/`strtod`, `qsort`/`bsearch`, `rand`, `time`/`localtime`, matematica |
| finestre (`user32`) | classi di finestra, `CreateWindowEx`, il ciclo dei messaggi (`GetMessage`, `PeekMessage`, `DispatchMessage`), timer, messaggi di mouse e tastiera, `WM_PAINT`, `MessageBox` |
| disegno (`gdi32`) | penne, pennelli e caratteri, rettangoli, ellissi, linee, poligoni, testo e `DrawText`, DC in memoria con `BitBlt` |

Un `.exe` con finestre (sottosistema GUI) si apre con un doppio clic senza
Terminale: le sue finestre sono finestre di justOS. Nell'immagine di disco,
che ha Wine, il doppio clic passa invece da Wine (vedi "Aprire con un doppio
clic"); `winrun` resta dove Wine non c'è (il CD live, `make WINE=0`) e dalla
shell. Una funzione importata che
`winrun` non conosce non ferma più il programma: lo stub lo dice una volta e
restituisce 0.

Mancano il registro, i processi e i thread veri, i controlli comuni
(`comctl32`), DirectX e i programmi a 32 bit.

Com'è fatto: `system/apps/winrun/src/pe.rs` (il caricatore),
`system/apps/winrun/src/win.rs` (`kernel32`), `crt.rs` (il runtime C) e
`gui.rs` (`user32` e `gdi32`).

### Wine

`winrun` basta ai programmi piccoli. I programmi Windows veri (controlli
comuni, registro, processi, installatori) li esegue **Wine 10** di Debian,
senza modifiche, sopra lo strato Linux. Wine è **nell'immagine di disco di
serie** (`make`, e `justos-img.zip` delle release), a 64 bit e con le DLL per
i programmi a 32 bit: un doppio clic su un `.exe` nell'app File lo avvia
senza installare nulla. Per questo l'immagine è da 5 GiB (vedi sotto).
`make WINE=0` costruisce l'immagine di prima, senza Wine, da 1 GiB; i test
usano quella (`build/justos-test.img`). Il **CD live non ha Wine**: il suo
volume di sistema sta in memoria e non porta le librerie di Debian, quindi i
programmi Windows (con Wine) girano solo dall'immagine di disco, installata
su una chiavetta o in una macchina virtuale.

Dalla shell:

```
cd sys:/usr/lib/wine
./wine64 notepad              il Blocco note di Wine
./wine64 winecfg              la configurazione di Wine
./wine64 cmd /c echo ciao     un programma da console
./wine64 "C:\Program Files\Windows NT\Accessories\wordpad.exe"
```

La prima volta Wine prepara il disco `C:` (il suo prefisso, in
`/home/.wine`): ci vuole circa un minuto, di più nell'emulatore (in QEMU
la finestra del Blocco note arriva in meno di due minuti), e una notifica lo
dice. Con un doppio clic su un `.exe` nell'app File (vedi "Aprire con un
doppio clic") Wine parte da solo.

**Quanto spazio.** I file di Wine sono la gran parte dell'immagine: dei
1,7 GiB occupati da un'immagine appena costruita, 1,3 GiB sono le DLL di
Wine (a 64 e a 32 bit). Il disco `C:` che Wine prepara al primo avvio le
copia in `system32` e `syswow64`: altri 1,4 GiB, 3,1 GiB in tutto. Per
questo l'immagine è da 5 GiB: dopo restano circa 1,8 GiB per i file e i
programmi dell'utente. `make SIZE=8G` la fa più grande. Preparare il disco
`C:` già quando si costruisce l'immagine risparmierebbe i due minuti del
primo avvio, ma servirebbe Wine sulla macchina che costruisce e l'immagine
porterebbe 1,4 GiB in più: per ora non si fa.

Le finestre passano per il driver Wayland di Wine e diventano **finestre di
justOS**. Wine disegna da sé la cornice di Windows, perché Wayland non gliene
offre una del server: il compositor la riconosce dai colori della barra del
titolo e la lascia fuori, così resta solo la cornice di justOS. Una cornice
da finestra di dialogo, più sottile, dice che la finestra non si
ridimensiona. `tools/linux/winelook.py` aggiunge al `wine.inf` di Wine, che
ogni prefisso installa, l'aspetto di justOS: colori scuri con l'arancione,
Google Sans Flex per l'interfaccia (menu, finestre di dialogo, messaggi) e
DejaVu Sans Mono per i caratteri a larghezza fissa e il Blocco note.

Provati: Blocco note, WordPad, Editor del Registro, winecfg, Task Manager,
WineMine, `cmd`. Mancano ancora Vulkan e DirectX accelerati, l'audio, gli
appunti condivisi con justOS e le icone nell'area di notifica.

**Programmi a 32 bit.** Wine li esegue nel suo modo "nuovo WoW64": il
caricatore resta a 64 bit e passa al codice a 32 bit cambiando segmento,
senza librerie Linux a 32 bit. Servono solo le DLL a 32 bit, che
`tools/linux/debroot.py` prende dal pacchetto `libwine` per i386 della stessa
versione (controllando lo SHA-256) e mette in
`/usr/lib/x86_64-linux-gnu/wine/i386-windows`. Il kernel fa la sua parte
con i selettori di Linux (0x23 codice a 32 bit, 0x2b dati, 0x33 codice a
64 bit), tre descrittori TLS per thread (`set_thread_area`, anche da
`int 0x80`) e i registri di segmento salvati a ogni cambio di thread
(`arch/gdt.rs`, `linux/thread.rs`).

```
./wine64 C:\windows\syswow64\cmd.exe /c echo ciao     console, a 32 bit
./wine64 C:\windows\syswow64\notepad.exe              con finestra, a 32 bit
```

Provati: `cmd` e il Blocco note a 32 bit. Un programma con licenza chiede
l'accesso con l'account di chi lo usa, come su Windows: justOS non aggira
nessun controllo. L'installatore web di AutoCAD 2027 parte ma si ferma dopo
pochi secondi, prima di mostrare una finestra (un suo programma a 64 bit
cade dentro Wine); AutoCAD recente non funziona con Wine nemmeno su Linux.

## Programmi Java e app Android

Le app Android e i programmi Java compilati per Android sono bytecode
**Dalvik** (file `.dex`, da soli o dentro un `.apk` o un `.jar`). Li esegue
`dalvik` (`system/apps/dalvik`), un interprete scritto da zero
(`system/libs/justdex`): legge il file DEX, esegue le istruzioni Dalvik e
fornisce la parte della libreria Java e del framework Android che serve.

```
Basics.dex                 un programma Java con main(): scrive nel Terminale
dalvik Input.dex           lo stesso, scritto per esteso
SelfTest.apk               un'app Android: si apre nella sua finestra
```

Un doppio clic su un `.apk` apre l'app nella sua finestra; un `.dex` o un
`.jar` parte nel Terminale. justOS riconosce il formato dai primi byte
(`dex\n` o un archivio zip con `classes.dex`).

| Area | Su justOS |
|---|---|
| linguaggio | tutte le istruzioni Dalvik: classi, interfacce, eccezioni con `try`/`catch`, array multidimensionali, `switch`, enum, classi interne e anonime |
| `java.lang` | `String`, `StringBuilder`, i tipi incapsulati, `Math`, `System`, `Character`, `Thread` (eseguito subito, non in parallelo), le eccezioni con lo stesso messaggio della JVM |
| `java.util` | liste, insiemi, mappe (anche ordinate), code, `Stack`, `Arrays`, `Collections`, `Random` (gli stessi numeri della JVM), `Scanner`, `Objects` |
| testo | `String.format`, espressioni regolari (`matches`, `split`, `replaceAll`), `BufferedReader` sulla tastiera |
| Android | `Activity` (anche `AppCompatActivity`) con il ciclo di vita, `TextView`, `Button`, `EditText`, `CheckBox`, `ProgressBar`, `ImageView`, `LinearLayout` e altri gruppi, `Toast`, `Handler`, `Log`, `Color` |

Le finestre delle app seguono lo stile Material chiaro: la barra con il
titolo, pulsanti a pillola, campi di testo con la riga sotto. Esc torna
indietro (`onBackPressed`), Tab passa da un campo all'altro. Se l'app si
ferma con un'eccezione, compare la scheda "*app* si è arrestata".

Il test `basics_print_what_java_prints` controlla che un programma di prova
scriva esattamente quello che scrive una JVM vera.

Com'è fatto: `system/libs/justdex/src/dex.rs` (il formato DEX),
`interp.rs` (le istruzioni), `java.rs` (la libreria Java), `text.rs` (numeri,
`format` ed espressioni regolari), `android.rs` (le viste e le attività) e
`system/apps/dalvik/src/screen.rs` (il disegno delle finestre).

### Verso Android vero: binder

`dalvik` basta alle app semplici. Le app vere vogliono il loro sistema:
ART, il framework e i servizi di Android, che parlano tra loro con
**binder**. Il kernel ha il driver binder di Linux (protocollo 8, a 64 bit;
`system/kernel/src/linux/binder.rs`), con i dispositivi `/dev/binder`,
`/dev/hwbinder` e `/dev/vndbinder`: il context manager (servicemanager),
transazioni a due vie e one-way (in fila per oggetto), chiamate annidate
che tornano al thread che aspetta, oggetti e handle con i loro conteggi
(`BR_INCREFS`, `BR_ACQUIRE` e i rilasci), descrittori installati in chi
riceve, buffer con puntatori (scatter-gather), notifiche di morte, la
richiesta di thread nuovi (`BR_SPAWN_LOOPER`) e `poll` sul dispositivo.
`make test-linux` lo prova con due processi (`scripts/linux/binder-test.c`).

Accanto c'è **`/dev/ashmem`**, la memoria condivisa di Android (libcutils la
usa ancora al posto di `memfd`): una regione con nome e dimensione, mappata
da chi riceve il descrittore, con la maschera dei permessi che li può solo
togliere (`system/kernel/src/linux/ashmem.rs`, provata da
`scripts/linux/ashmem-test.c`). Le aree delle proprietà di sistema sono file
mappati `MAP_SHARED`: init le scrive, gli altri processi le mappano in sola
lettura e aspettano i cambi con un futex nelle stesse pagine
(`scripts/linux/mapshared-test.c`). Sono i primi pezzi per eseguire
un'immagine di sistema Android sopra lo strato Linux, come fa Waydroid.

## Installare programmi

Il Negozio (`sys:/store`) ha, nella categoria *Sviluppatori*, due programmi
Linux liberi già compilati (vedi `sys:/store/SOURCES.md` per sorgenti e
licenze). Si installano dall'app Negozio o con il comando `install`, che li
copia in `sys:/apps`, da dove si avviano per nome:

```
install                    elenca i programmi disponibili
install busybox            installa BusyBox (decine di comandi Unix)
install lua                installa l'interprete Lua
uninstall lua              lo toglie
```

[BusyBox](https://busybox.net/) è un programma solo che, a seconda del nome con
cui lo si chiama, diventa uno di decine di comandi Unix. Dopo `install busybox`,
justOS mette a disposizione quei comandi (`cat`, `grep`, `wc`, `seq`, `sort`,
`ash`…): digitandone uno, la shell avvia `busybox` passandogli quel nome. I
comandi propri di justOS (per esempio `ls`) hanno la precedenza.

### Pacchetti con un doppio clic

Nell'app File un doppio clic su un pacchetto lo installa in `sys:/apps`, in
sottofondo, con una notifica alla fine (`system/kernel/src/store.rs`,
`install_file`; i formati sono in `system/libs/justzip`):

| Pacchetto | Cosa succede |
|---|---|
| `.deb` (Debian, Ubuntu) | si apre l'archivio `ar`, poi `data.tar.gz` (o `data.tar`); il programma in `usr/bin` (o simile) si installa se è un ELF x86-64 statico. `data.tar.xz` e `.zst` (quelli di Debian e Ubuntu) non sono ancora supportati: un doppio clic lo dice con una notifica |
| `.tar.gz`, `.tgz`, `.tar`, `.zip`, `.gz` | il primo programma Linux statico o Windows x64 dentro si installa |
| `.pkg` (macOS) | si apre l'archivio `xar`, poi il `Payload` (cpio, anche compresso gzip); i file vanno in `sys:/home/Applicazioni macOS/<nome>`. I `Payload` compressi con LZMA (`pbzx`, i più recenti) non sono ancora supportati |

Dal Terminale fa lo stesso `install <file>`, per esempio
`install sys:/home/Scaricati/ripgrep_14.1.0-1_amd64.deb`.

I programmi (ELF o `.exe`, riconosciuti dai primi byte anche senza
estensione) si avviano con un doppio clic come descritto qui sotto.

### Aprire con un doppio clic

Nell'app File un doppio clic sceglie cosa fare dal tipo del file
(`for_file` in `system/kernel/src/gui/apps/mod.rs`). Mentre un programma lento
parte, la sua icona salta nel dock finché non arriva la finestra.

| File | Cosa succede | Stato |
|---|---|---|
| `.exe` con finestre, con Wine (nell'immagine di disco di serie; non nel CD live né con `make WINE=0`) | parte con Wine, senza Terminale; la prima volta Wine prepara l'ambiente Windows (circa 2 minuti nell'emulatore, lo dice una notifica) | verificato: il Blocco note a 64 bit e un secondo programma |
| `.exe` a 32 bit, `.msi`, `.bat`, con Wine | stessa strada (`msiexec /i` per gli `.msi`; i `.bat` e i programmi da console girano nel Terminale) | non verificato con un doppio clic (a 32 bit funziona dalla shell) |
| `.exe` senza Wine (CD live, `make WINE=0`) | il piccolo `winrun`, come prima; `.msi` e `.bat` dicono che serve Wine | non verificato con un doppio clic |
| programma Linux con finestra (collegato a GTK, Qt, SDL, Wayland o X11) | parte senza Terminale | verificato |
| programma Linux da riga di comando | si apre nel Terminale | come prima |
| `.desktop` | parte il programma del suo `Exec` | verificato |
| AppImage | si estrae e parte da lì (`--appimage-extract-and-run`: FUSE non c'è); ci vuole un po' | verificato con una AppImage |
| `.deb` con `data.tar.xz` o `.zst` (quelli di Debian e Ubuntu) | una notifica dice che non si installa ancora (manca il decompressore) | non funziona |
| `.deb` con gzip | va all'installatore, come prima | come prima |
| `.html`, `.htm` e link | si aprono in Firefox, se c'è; sennò i file nell'editor, e per i link una notifica dice che serve Firefox | verificato con un file .html; se Firefox è già aperto una seconda pagina non gli si può passare (manca D-Bus) (la notifica prevista non è comparsa nelle prove) |
| `.dmg`, programmi Mach-O, cartelle `.app` di macOS | "Le app per macOS non funzionano su justOS." | non funziona |
| `.pkg` | l'installatore lo scompatta, come prima | come prima |
| `.apk`, `.dex`, `.jar` | invariato (vedi sopra) | come prima |

Wine e Firefox vedono solo i file del volume di sistema (`sys:`).

### Cosa non può girare

- **macOS**: i programmi sono in formato Mach-O e usano le librerie di
  sistema di Apple (Cocoa, Metal…), che non si possono ridistribuire: un
  `.pkg` si scompatta, ma il programma non parte. Lo stesso vale per i `.dmg`
  e per le cartelle `.app`: un doppio clic lo dice con un messaggio.
- **Gran parte delle app Android del Play Store**: girano le app che
  costruiscono l'interfaccia nel codice. Mancano i layout XML e le risorse
  (`R.layout`, `R.string`: `setContentView(R.layout.x)` si ferma con un
  messaggio chiaro), le viste personalizzate con `Canvas`, i `Fragment`, la
  rete, i servizi di Google Play, il codice nativo (`.so`) e le lambda
  compilate con `invokedynamic` (quelle trasformate da `d8` vanno bene). I
  thread girano uno dopo l'altro.
- **Linux**: `execve` da un programma con più thread e il controllo dei job
  (`SIGSTOP`, `SIGTSTP`) non ci sono ancora. Le finestre si disegnano con il processore
  (niente OpenGL o Vulkan, niente `linux-dmabuf`) e non c'è un server X11:
  servono programmi che parlano Wayland.
- **Windows a 32 bit**: solo con Wine (nell'immagine di disco, non nel CD live); `winrun` esegue solo programmi a 64 bit.

## Provarlo

`make test-compat` compila al volo un piccolo programma Linux (con `musl-gcc`) e
alcuni Windows (con `x86_64-w64-mingw32-gcc`: console, file, tastiera e una
finestra), li mette su una copia dell'immagine insieme ai programmi del
negozio e ai programmi Java e Android di prova, avvia justOS e controlla che
funzionino. Se uno dei due compilatori non c'è, quella parte viene saltata (e
segnalata). I file `.dex` e `.apk` di prova sono già nel repository
(`system/libs/justdex/tests`); `build.sh` li ricrea con un JDK e `dx`.

Per compilare a mano un programma da provare:

```
musl-gcc -static -Os -o hello hello.c            # Linux
x86_64-w64-mingw32-gcc -O2 -o hello.exe hello.c  # Windows
```

poi copiarlo nell'immagine con `justtool fs` (vedi `docs/programmi.md`).
