# Roadmap: un arcOS che si usa come Windows e macOS

L'obiettivo è «zero attrito»: chi arriva da Windows o da macOS deve trovare
le cose dove se le aspetta, senza terminale e senza configurare niente. Questo
documento dice, per ogni area, **cosa c'è oggi**, **cosa manca**, con quali
**interfacce** (chiamate di sistema, IPC, librerie) e in che **ordine** farlo.
Le stime sono oneste: alcune cose sono settimane, altre anni, una non si può
fare.

Legenda: ✅ fatto · 🟡 in parte · ⬜ da fare · ⛔ non realizzabile.

## Le basi comuni

Quasi tutto quello che segue ha bisogno di tre fondamenta che oggi mancano o
sono minime. Vanno fatte per prime.

### 1. IPC: porte di messaggi ✅

I programmi e i servizi del kernel si parlano con **porte** (sul modello
delle porte Mach di macOS e di Fuchsia, ma piccole): `kernel/src/ipc.rs`,
chiamate 31–36 in `arcdk::syscall`, libreria `arcrt::ipc`.

| Chiamata | Cosa fa |
|---|---|
| `PORT_CREATE(name) -> handle` | crea una porta; un nome (`arc.notify`) la rende trovabile, senza nome è privata |
| `PORT_LOOKUP(name) -> handle` | un handle per inviare a una porta con nome |
| `PORT_SEND(handle, buf, len)` | mette in coda un messaggio (fino a 64 KiB, 64 in coda) |
| `PORT_RECV(handle, buf, cap, timeout, info)` | riceve da una porta propria; `info`: lunghezza, pid del mittente (garantito dal kernel), handle per la risposta |
| `PORT_CALL(handle, req, len, resp, cap)` | richiesta e risposta in una volta, con una porta di risposta privata |
| `PROC_NAME(pid, buf, cap)` | il nome di un processo (chi ha inviato un messaggio) |

Riceve solo chi ha creato la porta; inviare può chiunque ne abbia un
handle. Le porte si contano: quando il proprietario termina la porta sparisce
e chi invia riceve `Closed`; se un servizio termina senza rispondere, chi
aspettava la risposta riceve `Closed` invece di restare bloccato. Le attese
si accorgono di un Ctrl+C. Il primo servizio è `arc.notify` nel kernel
(`{"title": …, "text": …}` mostra una notifica); `ports` nella shell elenca
le porte con nome, `ipc test` le prova (anche in `make test`).

Ancora da fare: allegare handle qualsiasi ai messaggi (oggi viaggia solo
quello della risposta) e decidere chi può trovare quale porta, che arriva
con l'identità delle app.

### 2. Identità delle app e sandbox

Ogni app installata ha un `app_id` (`org.arcos.counter`: ArcScript lo usa
già nei pacchetti `.arc`). Il kernel associa l'`app_id` al processo quando
lo avvia da `sys:/apps` e da lì decide cosa può fare:

- file: solo la propria cartella `sys:/home/.apps/<app_id>` e ciò che
  l'utente sceglie con il selettore di file del sistema (un *portale*, come
  in Flatpak e nella sandbox di macOS: il selettore gira fuori dall'app e
  le consegna un handle);
- dispositivi (fotocamera, microfono, posizione) e rete: solo con un
  permesso concesso.

### 3. Grafica accelerata

Oggi il desktop disegna tutto con il processore (`arcgfx`), con
aggiornamenti per rettangoli sporchi e animazioni a tempo: fluido a
1024×768 e 1080p su una macchina moderna, ma il processore lavora per ogni
pixel. Driver video: `bochsvga`, `virtiogpu`, `vmsvga`, `cirrus` e il
framebuffer UEFI, tutti senza accelerazione 3D.

## 1. Grafica e desktop

| Cosa | Stato |
|---|---|
| Compositore con rettangoli sporchi, sfocatura, ombre, animazioni a tempo | ✅ (via processore) |
| 60 fps nelle animazioni | 🟡 sì a risoluzioni normali; non garantito in 4K |
| 120 Hz, VRR | ⬜ |
| HiDPI (scala 2×) | ⬜ |
| Più monitor | ⬜ |
| Dock, Applicazioni (Launchpad), barra in alto, centro di controllo | ✅ |
| Centro notifiche con cronologia e *Non disturbare* | ✅ |
| Ricerca globale (app, file, impostazioni, calcoli) con Ctrl+Spazio | ✅ |
| Snap delle finestre (metà, quarti, tutto schermo) da mouse e tastiera | ✅ |
| Desktop virtuali e vista d'insieme (F3) | ✅ fino a sei, con trascinamento delle finestre |
| Gesti del touchpad | ⬜ (manca un driver touchpad multi-touch) |

**Librerie e interfacce.** `arcgfx` (disegno), `gui/wm.rs` (`App`,
`Window`), `gui/desktop.rs` (compositore e shell); per i programmi le
chiamate `WIN_CREATE`/`WIN_PRESENT`/`WIN_EVENT`.

**Passi.**

1. ~~**Desktop virtuali**~~ fatto: `Window::space`, la vista d'insieme
   disegna ogni finestra una volta con `Window::snapshot` e la scala.
2. **HiDPI** (3–4 settimane): un fattore di scala nel desktop; l'atlante dei
   font (`tools/assets/gen_fonts.py`) generato anche a 2×, le icone sono già
   grandi. Le coordinate delle app restano logiche; `WIN_PRESENT` riceve
   buffer a 2×.
3. **Più monitor** (1–2 mesi): `dev::display` con più uscite (virtio-gpu ne
   ha fino a 16, il driver Intel le avrebbe); il desktop diventa una lista
   di schermi con una barra ciascuno.
4. **Accelerazione** (molti mesi): prima `virtio-gpu` con *virgl* (OpenGL
   tramite l'host, buona per le macchine virtuali), poi un driver nativo.
   Su hardware vero l'unica strada realistica è portare i driver DRM di
   Linux (i915/amdgpu: milioni di righe) dietro uno strato di compatibilità,
   oppure Mesa con un backend per arcOS. È il lavoro più grande della lista.
5. **Gesti** (3–4 settimane dopo un driver I²C-HID/PS/2 multi-touch): tre
   dita per Mission Control e per cambiare desktop, due per scorrere.

## 2. Compatibilità con le app

| Cosa | Stato |
|---|---|
| Browser | 🟡 ticly browser con `arcweb`: HTML, CSS, immagini, moduli, cookie, HTTPS (`arctls`), JavaScript (`arcjs`, interprete senza JIT) |
| WebGL, DRM (Widevine) | ⬜ / ⛔ |
| Negozio grafico con installazione in un clic | ✅ (`sys:/store`, pacchetti `.app`) |
| Aggiornamenti delle app in background | ⬜ |
| Permessi in stile mobile | ⬜ (dopo IPC e sandbox) |
| Programmi Linux (ELF) | 🟡 solo collegati staticamente (musl), senza `ld.so` (`kernel/src/linux`, `make test-linux`) |
| Programmi Windows (PE) | 🟡 console e finestre semplici (`winrun`) |
| Android (DEX/APK) | 🟡 `dalvik`, programmi semplici |
| ArcScript con IDE (ArcStudio) | ✅ |

**Il browser, onestamente.** Un motore moderno (Chromium/Blink, WebKit,
Gecko) è decine di milioni di righe di C++ e chiede: thread POSIX, memoria
virtuale con `mmap`/`mprotect` (il JIT di JavaScript), socket, font con
FreeType/HarfBuzz, una GPU per il compositing. La strada praticabile:

1. completare lo strato Linux finché `musl` + programmi C++ complessi
   girano (librerie condivise con `ld.so`, thread, `futex`, `mmap` con
   permessi, `epoll`, socket);
2. portare **WebKitGTK** o **Servo** sopra quello strato, disegnando in una
   finestra `WIN_*` (Servo è Rust e più piccolo: è la scommessa migliore);
3. ✅ JavaScript intanto: `arcjs`, un interprete scritto da zero in Rust,
   con il DOM di `arcweb` (eventi, timer, `fetch`); basta per i siti
   semplici, non per le applicazioni web pesanti.

**Widevine non si può avere**: Google lo concede solo ai produttori di
browser e sistemi con cui ha un contratto, ed è un binario chiuso per
Windows/macOS/Linux/Android. Senza, Netflix, Disney+ e simili non
riprodurranno video in alta definizione (alcuni per nulla). È un limite da
dichiarare, come per ogni sistema non commerciale.

**Negozio e aggiornamenti.** Il Negozio installa già da `sys:/store`; il
controllo degli aggiornamenti del sistema chiede le release a GitHub
(`update.rs`). Passi:

1. un indice remoto firmato (`index.json` + firma Ed25519, `arctls` ha già le
   primitive) con versioni e hash delle app;
2. un servizio `arc.updates` che una volta al giorno confronta l'indice con
   le app installate e scarica in background; la sostituzione avviene alla
   chiusura dell'app;
3. i permessi dichiarati nel manifest (`permissions = ["camera",
   "files:Immagini"]`) mostrati nella pagina del Negozio.

## 3. Plug and play

| Cosa | Stato |
|---|---|
| USB (UHCI, OHCI, EHCI, xHCI), tastiera, mouse, tavoletta, chiavette | ✅ |
| Dischi SATA, NVMe, virtio, schede SD | ✅ |
| Ethernet (e1000, igb, rtl8139/8169, pcnet, virtio-net) con DHCP | ✅ |
| Audio (AC'97, HD Audio) | ✅ riproduzione |
| Collegamento a caldo dell'audio (cuffie) | ⬜ |
| Wi-Fi | ⬜ |
| Bluetooth | ⬜ |
| Sospensione in RAM (S3/s2idle) | ⬜ |
| Batteria e temperatura | ⬜ |

**Wi-Fi.** Ogni famiglia di schede (Intel iwlwifi, Realtek, MediaTek,
Broadcom) ha un driver da decine di migliaia di righe e un firmware binario
da caricare; poi servono WPA2/WPA3 (il supplicant: handshake a 4 vie, SAE).
Piano: un solo chip, **Intel AX200/AX210** (il più diffuso nei portatili),
con il firmware ufficiale ridistribuibile; il supplicant in Rust sopra
`arctls`; un servizio `arc.net` con porte IPC `scan`, `connect`, `status`
e il menu nella barra (che c'è già per lo stato della rete). Stima: 4–6
mesi per un chip.

**Bluetooth.** Più semplice del Wi-Fi a livello di driver (USB HCI è
standard: un driver vale per quasi tutti i dongle), più complicato sopra:
L2CAP, profili HID (tastiere, mouse) e A2DP (audio, con il codec SBC).
Stima: 2–3 mesi per HID, altrettanti per l'audio.

**Sospensione.** Richiede l'interprete AML di ACPI (oggi arcOS legge le
tabelle ma non esegue AML): metodi `_PTS`, `_S3`, `_WAK`, poi ogni driver
deve salvare e ripristinare lo stato del suo dispositivo (`suspend`/`resume`
nell'interfaccia dei driver `arcdrv`). Ripresa sotto il secondo è
realistica dopo; il lavoro grosso è l'interprete AML (si può portare
`acpica`, in C, o usare il crate `aml`). Batteria e temperatura vengono
dallo stesso interprete (`_BST`, `_TMP`). Stima: 3–4 mesi.

## 4. File manager e ricerca

| Cosa | Stato |
|---|---|
| File: risorse, cartelle, apertura con l'app giusta, rinomina, Cestino | ✅ |
| Anteprime (miniature di immagini) | 🟡 nelle Foto; in File icone per tipo |
| Schede | ✅ Ctrl+T, Ctrl+W, Ctrl+Tab |
| Trascinamento tra finestre | ⬜ |
| Ricerca globale istantanea | ✅ per nome, per la home |
| Ricerca nel contenuto dei file | ⬜ |

**Passi.**

1. ~~**Schede in File**~~ fatto.
2. **Anteprime** (2 settimane): miniature QOI già decodificate dalle Foto
   (`gui/res.rs`), prima pagina dei testi, *Quick Look* con la barra
   spaziatrice.
3. **Trascinamento** (2–3 settimane): un `Drag::Files(Vec<String>)` nel
   desktop, consegnato alla finestra sotto il puntatore con un nuovo metodo
   `App::drop(paths, x, y)`; per i programmi un evento `WIN_EV_DROP`.
4. **Indice** (1 mese): un servizio che tiene un indice dei nomi e delle
   parole dei file di testo, aggiornato dal file system (ArcFS segnala le
   scritture a un osservatore: nuova chiamata `WATCH(path)`); la ricerca di
   Ctrl+Spazio lo interroga invece di leggere la home.

## 5. Aggiornamenti e sicurezza

| Cosa | Stato |
|---|---|
| Avviso di una nuova versione (release di GitHub) | ✅ |
| Aggiornamento atomico con ritorno indietro | ⬜ |
| Password con sale e 20 000 passaggi di SHA-256, blocco automatico | ✅ |
| Richieste di permesso (fotocamera, microfono, cartelle) | ⬜ |

**Aggiornamenti atomici A/B.** Il disco ha già una partizione EFI e una
ArcFS; il kernel e i file di sistema vanno in due partizioni di sistema (A
e B) e i dati restano in una partizione utente:

1. l'aggiornamento scarica l'immagine firmata nella partizione inattiva,
   ne verifica l'hash;
2. scrive nella partizione EFI quale avviare la prossima volta, con un
   contatore di tentativi;
3. al riavvio il sistema nuovo, se arriva al desktop, si dichiara buono;
   se no, dopo due tentativi ArcBoot torna all'altro (come Android e
   ChromeOS).

ArcBoot (BIOS e UEFI, `boot/`) va esteso a leggere lo slot e il contatore.
Stima: 1–2 mesi. Gli snapshot del file system (alla APFS o Btrfs) sono
un'alternativa più elegante ma richiedono il copy-on-write in ArcFS.

**Permessi.** Dopo IPC e identità delle app: un servizio `arc.permissions`
con la tabella delle concessioni (`sys:/system/permissions.json`); quando
un'app apre la fotocamera o una cartella fuori dalla sua, il kernel sospende
la chiamata e chiede al servizio, che mostra la finestra «*Counter* vuole
usare la fotocamera — Consenti / Non consentire» e ricorda la risposta;
*Impostazioni → Privacy* elenca e revoca.

## Ordine consigliato

| Fase | Contenuto | Durata indicativa |
|---|---|---|
| 1 | identità delle app (IPC a porte, desktop virtuali e schede in File: fatti) | 2 mesi |
| 2 | sandbox e permessi, aggiornamenti delle app in background, trascinamento | 2–3 mesi |
| 3 | interprete AML: sospensione, batteria, temperatura | 3–4 mesi |
| 4 | aggiornamenti A/B, HiDPI, indice dei file | 2–3 mesi |
| 5 | strato Linux completo (JavaScript in `arcweb`: fatto), poi Servo | 6–12 mesi |
| 6 | Wi-Fi (un chip), Bluetooth HID e audio | 6–9 mesi |
| 7 | accelerazione grafica (virgl, poi hardware vero) | 12 mesi e oltre |

Ognuna è utilizzabile appena finita: niente aspetta l'ultima.
