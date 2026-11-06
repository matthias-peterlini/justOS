# Programmi Linux, Windows e Android su JustOS

JustOS esegue, oltre ai suoi programmi nativi e a quelli ArcScript, anche
programmi **Linux** e **Windows** per x86-64 e app **Android** e programmi
Java (vedi [più sotto](#programmi-java-e-app-android)), senza modificarli. Non c'è una
macchina virtuale: il programma gira nello stesso modo degli altri, in ring 3
e nel suo spazio di indirizzi; quando chiede qualcosa al sistema, JustOS traduce
la richiesta nelle proprie chiamate.

L'idea è la stessa di [Wine](https://www.winehq.org/) e di
[WSL](https://learn.microsoft.com/windows/wsl/), in piccolo: nessun codice di
Microsoft o di Linux è copiato in JustOS. Il supporto è scritto da zero a
partire dalle interfacce pubbliche (la ABI di System V per Linux, la
convenzione x64 e i pochi `.dll` di sistema per Windows).

## Programmi Linux

JustOS carica un eseguibile ELF x86-64 e serve le chiamate di sistema Linux
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
dipendenze e li scompatta in `build/linux`. L'immagine li porta nel volume di
sistema con la disposizione di Debian: tutto in `/usr`, con `/bin`, `/sbin`,
`/lib` e `/lib64` come collegamenti simbolici dentro `/usr` (JustFS ha i
collegamenti simbolici). L'app File non mostra queste cartelle. Quali
pacchetti, lo dice `LINUX_PKGS` nel `Makefile` (per ora `libc6`, la glibc
2.41 di Debian 13); `make linux-root` rifà l'albero.

Un programma Linux si avvia come gli altri, per percorso o per nome:

```
sys:/home/hello            un eseguibile Linux qualunque
busybox ls -la             un programma installato (vedi "Installare programmi")
```

### Cosa vede il programma

| Area | Su JustOS |
|---|---|
| `stdin`, `stdout`, `stderr` | la console; `stdin` legge una riga per volta, con l'eco dei tasti |
| file e cartelle | i volumi montati: il "/" di Linux è la radice del volume corrente, così `/home/x` è `<volume>:/home/x` |
| memoria | `brk` e `mmap` anonimo (il programma alloca e libera normalmente) |
| orologio | `clock_gettime`, `gettimeofday`, `nanosleep` |
| TLS | `arch_prctl(ARCH_SET_FS)` imposta la base del segmento FS del thread |
| casualità | `getrandom` |
| librerie condivise | il caricatore dinamico della glibc, `mmap` dei file (i byte di un file mappato più volte sono condivisi), `dlopen` |
| thread | `clone` per i thread, `futex`, `set_tid_address` |
| rete | socket TCP e UDP, `poll`, `select`, `epoll` |
| collegamenti simbolici | `symlink`, `readlink`, `lstat` |
| utenti | tutto gira come `root` |
| processi nuovi | `fork` (il figlio ha una copia della memoria), `vfork` e `posix_spawn` (il figlio usa la memoria del padre finché non chiama `execve`), `execve` (anche degli script che iniziano con `#!`), `wait4`, `waitid`, `getppid`; i descrittori aperti passano al figlio, quelli con `O_CLOEXEC` si chiudono con `execve` |
| segnali | per ora solo verso sé stessi: un segnale mandato a un altro processo non arriva |

Le chiamate implementate bastano ai programmi da riga di comando: lettura e
scrittura di file, elenco di cartelle (`getdents64`), `stat`, `ioctl` per sapere
se l'uscita è un terminale, e così via. Una chiamata non ancora prevista compare
nel giornale di sistema come `unimplemented syscall N` (con `verbose on`), così
è facile vedere cosa manca.

Com'è fatto: `system/kernel/src/linux/` (il caricamento e le chiamate) e
`system/kernel/src/arch/syscall.rs` (l'istruzione `syscall` e le basi FS/GS di
ogni thread).

## Programmi Windows

I programmi Windows li esegue `winrun`, un programma di JustOS
(`system/apps/winrun`). `winrun` carica l'eseguibile `.exe` (PE32+ per x64),
mette i propri indirizzi nella tabella delle importazioni al posto delle
funzioni di `kernel32.dll` e `msvcrt.dll`, prepara un blocco di thread (TEB) e
salta al punto d'ingresso. Il programma gira nello stesso processo di `winrun`:
quando chiama, per esempio, `printf`, chiama il codice di `winrun`, che a sua
volta usa le chiamate di JustOS.

Un `.exe` si avvia per nome (JustOS riconosce l'intestazione `MZ` e usa `winrun`
da solo) o esplicitamente:

```
hello.exe                  JustOS vede che è un programma Windows e lo avvia
winrun hello.exe alpha     lo stesso, scritto per esteso
```

Sono gestiti i **programmi x64** costruiti con
[MinGW-w64](https://www.mingw-w64.org/) (`x86_64-w64-mingw32-gcc`), da console
e con finestre:

| Area | Su JustOS |
|---|---|
| console | i tre flussi standard, `printf`/`scanf` e famiglia, `fgets(stdin)`, `getchar`, `ReadConsole` |
| file | `fopen`/`fread`/`fgets`/`fscanf`/`fseek` e `CreateFile`/`ReadFile`/`WriteFile`; i percorsi Windows (`C:\x`) diventano percorsi di JustOS |
| runtime C | memoria (`malloc`, `HeapAlloc`, `VirtualAlloc`), stringhe, `ctype`, `strtol`/`strtod`, `qsort`/`bsearch`, `rand`, `time`/`localtime`, matematica |
| finestre (`user32`) | classi di finestra, `CreateWindowEx`, il ciclo dei messaggi (`GetMessage`, `PeekMessage`, `DispatchMessage`), timer, messaggi di mouse e tastiera, `WM_PAINT`, `MessageBox` |
| disegno (`gdi32`) | penne, pennelli e caratteri, rettangoli, ellissi, linee, poligoni, testo e `DrawText`, DC in memoria con `BitBlt` |

Un `.exe` con finestre (sottosistema GUI) si apre con un doppio clic senza
Terminale: le sue finestre sono finestre di JustOS. Una funzione importata che
`winrun` non conosce non ferma più il programma: lo stub lo dice una volta e
restituisce 0.

Mancano il registro, i processi e i thread veri, i controlli comuni
(`comctl32`), DirectX e i programmi a 32 bit.

Com'è fatto: `system/apps/winrun/src/pe.rs` (il caricatore),
`system/apps/winrun/src/win.rs` (`kernel32`), `crt.rs` (il runtime C) e
`gui.rs` (`user32` e `gdi32`).

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
`.jar` parte nel Terminale. JustOS riconosce il formato dai primi byte
(`dex\n` o un archivio zip con `classes.dex`).

| Area | Su JustOS |
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
JustOS mette a disposizione quei comandi (`cat`, `grep`, `wc`, `seq`, `sort`,
`ash`…): digitandone uno, la shell avvia `busybox` passandogli quel nome. I
comandi propri di JustOS (per esempio `ls`) hanno la precedenza.

### Pacchetti con un doppio clic

Nell'app File un doppio clic su un pacchetto lo installa in `sys:/apps`, in
sottofondo, con una notifica alla fine (`system/kernel/src/store.rs`,
`install_file`; i formati sono in `system/libs/justzip`):

| Pacchetto | Cosa succede |
|---|---|
| `.deb` (Debian, Ubuntu) | si apre l'archivio `ar`, poi `data.tar.gz` (o `data.tar`); il programma in `usr/bin` (o simile) si installa se è un ELF x86-64 statico. `data.tar.xz` e `.zst` non sono ancora supportati |
| `.tar.gz`, `.tgz`, `.tar`, `.zip`, `.gz` | il primo programma Linux statico o Windows x64 dentro si installa |
| `.pkg` (macOS) | si apre l'archivio `xar`, poi il `Payload` (cpio, anche compresso gzip); i file vanno in `sys:/home/Applicazioni macOS/<nome>`. I `Payload` compressi con LZMA (`pbzx`, i più recenti) non sono ancora supportati |

Dal Terminale fa lo stesso `install <file>`, per esempio
`install sys:/home/Scaricati/ripgrep_14.1.0-1_amd64.deb`.

Un doppio clic su un programma (ELF o `.exe`, riconosciuti dai primi byte
anche senza estensione) lo avvia nel Terminale.

### Cosa non può girare

- **macOS**: i programmi sono in formato Mach-O e usano le librerie di
  sistema di Apple (Cocoa, Metal…), che non si possono ridistribuire: un
  `.pkg` si scompatta, ma il programma non parte. Lo stesso vale per i `.dmg`.
- **Gran parte delle app Android del Play Store**: girano le app che
  costruiscono l'interfaccia nel codice. Mancano i layout XML e le risorse
  (`R.layout`, `R.string`: `setContentView(R.layout.x)` si ferma con un
  messaggio chiaro), le viste personalizzate con `Canvas`, i `Fragment`, la
  rete, i servizi di Google Play, il codice nativo (`.so`) e le lambda
  compilate con `invokedynamic` (quelle trasformate da `d8` vanno bene). I
  thread girano uno dopo l'altro.
- **Linux**: i segnali tra processi (`kill`, `SIGCHLD`), `execve` da un
  programma con più thread, la memoria condivisa (`MAP_SHARED` tra processi,
  `memfd`) e i socket `AF_UNIX` non ci sono ancora.
- **Windows a 32 bit**: non ancora.

## Provarlo

`make test-compat` compila al volo un piccolo programma Linux (con `musl-gcc`) e
alcuni Windows (con `x86_64-w64-mingw32-gcc`: console, file, tastiera e una
finestra), li mette su una copia dell'immagine insieme ai programmi del
negozio e ai programmi Java e Android di prova, avvia JustOS e controlla che
funzionino. Se uno dei due compilatori non c'è, quella parte viene saltata (e
segnalata). I file `.dex` e `.apk` di prova sono già nel repository
(`system/libs/justdex/tests`); `build.sh` li ricrea con un JDK e `dx`.

Per compilare a mano un programma da provare:

```
musl-gcc -static -Os -o hello hello.c            # Linux
x86_64-w64-mingw32-gcc -O2 -o hello.exe hello.c  # Windows
```

poi copiarlo nell'immagine con `justtool fs` (vedi `docs/programmi.md`).
