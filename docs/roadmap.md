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

Con l'identità delle app, un'app nella sandbox può creare solo porte con il
proprio nome (`<app_id>` o `<app_id>.*`); i nomi `arc.*` restano al
sistema. Ancora da fare: allegare handle qualsiasi ai messaggi (oggi viaggia
solo quello della risposta).

### 2. Identità delle app e sandbox ✅

Ogni app installata ha un `app_id` (`kernel/src/sandbox.rs`, chiamate 37–39
in `arcdk::syscall`, libreria `arcrt::app`):

- le app native in `sys:/apps` lo prendono dalla chiave `app_id=` del loro
  `<nome>.info` (altrimenti `org.arcos.<nome>`), i pacchetti `.arc` da
  `arcc`, che chiama `APP_ENTER` prima di eseguirli; i processi figli
  ereditano l'app del padre. Gli id sono minuscoli e `arc.` è riservato.
- file: la propria cartella `sys:/home/.apps/<app_id>` (anche la cartella di
  partenza), in sola lettura i caratteri, i certificati, i suoni e le app;
  le cartelle della home (Documenti, Immagini, Musica…) e gli altri dischi
  solo con un permesso (`files:Immagini`, `files:ram:`); tutto il resto è
  negato con `Denied`.
- il selettore di file del sistema (`PICK_FILE`, un *portale* come in Flatpak
  e nella sandbox di macOS) gira fuori dall'app: il file scelto dall'utente
  diventa accessibile all'app per il resto della sua vita, senza altri
  permessi.
- dispositivi (fotocamera, microfono, posizione) e rete: si chiedono al
  servizio `arc.permissions` (`{"request": "camera"}` → `{"granted": …}`).

La prima volta il sistema chiede con una finestra «Privacy» (Consenti / Non
consentire); la risposta resta in `sys:/system/permissions.json`. *Impostazioni
→ Sistema → Privacy delle app* elenca le risposte, le cambia e le dimentica;
dal terminale lo fa `perms`. Senza interfaccia grafica le richieste nuove
sono negate con un suggerimento (`perms allow <app> <permesso>`). `sandbox
test` prova tutto (anche in `make test`). Disegno salva in Immagini (dopo il
permesso) e con Ctrl+Maiusc+S / Ctrl+O usa il selettore; 2048 e Serpente
tengono il record nella propria cartella.

Limiti onesti: i programmi Linux e Windows non sono nella sandbox (girano
come programmi di sistema); per fotocamera, microfono e posizione c'è il
permesso ma non ancora i driver; il file scelto nel selettore vale solo
finché l'app resta aperta.

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
| Aggiornamenti delle app in background | ✅ indice firmato Ed25519, servizio `arc.updates`, sezione Aggiornamenti del Negozio |
| Permessi in stile mobile | ✅ cartelle, dischi, fotocamera, microfono, posizione, rete (`arc.permissions`) |
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

**Negozio e aggiornamenti.** Fatto:

1. ✅ un indice remoto firmato: `make updates` scrive `index.json` (numero
   di serie, versione, URL, SHA-256 e dimensione di ogni app) e la sua
   firma Ed25519 (`arctls::ed25519`); `scripts/publish-updates.sh` lo
   pubblica sul ramo `updates` del repository. arcOS rifiuta un indice con
   una firma sbagliata o un numero di serie più vecchio dell'ultimo visto;
2. ✅ il servizio `arc.updates` (`kernel/src/appupdate.rs`) controlla una
   volta al giorno, scarica in background e sostituisce un'app quando è
   chiusa; le app installate da GitHub si confrontano con l'ultima release;
3. ✅ i permessi dichiarati nel manifest (`permissions=camera,files:Immagini`
   nel file `info`) sono mostrati nella pagina del Negozio; un
   aggiornamento che ne chiede di nuovi aspetta il consenso.

Manca: la firma anche per i programmi Linux e Windows (oggi solo HTTPS) e
il ritorno alla versione precedente.
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
| Sospensione in RAM | ✅ S3, con tutti i driver inclusi; s2idle ⬜ |
| Batteria e temperatura | ✅ interprete AML `arcaml`, embedded controller, GPE |

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

**Batteria e temperatura.** Fatto: arcOS esegue l'AML del firmware con il
suo interprete, `arcaml` (scritto da zero, provato con le tabelle di sette
portatili veri e di QEMU), e da lì legge batteria, alimentatore, coperchio
e temperature; gli eventi arrivano dai GPE e dall'embedded controller.
Vedi [architettura](architettura.md#acpi-e-aml).

**Sospensione.** Fatto: S3 dal menu, dal tasto di sospensione, dal
coperchio e con `suspend` nel Terminale; il risveglio passa dal vettore
della FACS al trampolino dei processori secondari, poi `_WAK`, PCI,
interruzioni, processori e dispositivi. Ogni driver incluso ha i suoi
hook (`power_hooks`, vedi [driver](driver.md#sospensione)); al risveglio
c'è la schermata di blocco. Provata in QEMU su PIIX4, Q35 e UEFI, con
due processori, con ogni disco, scheda di rete, controller USB, scheda
audio, lettore SD e schermo che QEMU emula. Mancano: s2idle (i portatili
recenti senza S3), la prova su portatili veri e lo stato dei dispositivi
PCI che il firmware non ripristina da sé (`_PS0`/`_PS3`, D3cold).

## 4. File manager e ricerca

| Cosa | Stato |
|---|---|
| File: risorse, cartelle, apertura con l'app giusta, rinomina, Cestino | ✅ |
| Anteprime (miniature di immagini) | 🟡 nelle Foto; in File icone per tipo |
| Schede | ✅ Ctrl+T, Ctrl+W, Ctrl+Tab |
| Trascinamento tra finestre | ✅ File, Terminale, Editor, dock (Cestino e app), programmi con `WIN_DROPS` |
| Selezione multipla | ✅ Ctrl+clic, Maiusc+clic, Ctrl+A |
| Ricerca globale istantanea | ✅ per nome, per la home |
| Ricerca nel contenuto dei file | ⬜ |

**Passi.**

1. ~~**Schede in File**~~ fatto.
2. **Anteprime** (2 settimane): miniature QOI già decodificate dalle Foto
   (`gui/res.rs`), prima pagina dei testi, *Quick Look* con la barra
   spaziatrice.
3. ~~**Trascinamento**~~ fatto: i file trascinati fuori da File li porta il
   desktop (`gui/dnd.rs`), con l'icona sotto il puntatore; la finestra sotto
   li riceve (`App::drop_files`) e illumina dove andrebbero. Dentro un
   volume si spostano, verso un altro si copiano; Ctrl o Alt copia sempre,
   Maiusc sposta sempre; un nome già usato diventa «foto 2.qoi»; una
   cartella non va mai dentro sé stessa. Sul Terminale si scrivono i
   percorsi, sull'Editor si apre il file, sull'icona del Cestino si
   cestinano, su quella di un'app si aprono con lei. I programmi creano la
   finestra con `WIN_DROPS`, ricevono `WIN_EV_DROP` e leggono i percorsi
   con `WIN_DROPPED`: la sandbox lascia leggere (non scrivere) i file
   rilasciati. Prova automatica: `dnd test`. Manca: trascinare *verso* File
   da un programma, e il rettangolo di selezione.
4. **Indice** (1 mese): un servizio che tiene un indice dei nomi e delle
   parole dei file di testo, aggiornato dal file system (ArcFS segnala le
   scritture a un osservatore: nuova chiamata `WATCH(path)`); la ricerca di
   Ctrl+Spazio lo interroga invece di leggere la home.

## 5. Aggiornamenti e sicurezza

| Cosa | Stato |
|---|---|
| Avviso di una nuova versione (release di GitHub) | ✅ |
| Aggiornamento atomico con ritorno indietro | ✅ due slot di sistema, prova e conferma, ritorno automatico (`make test-update`) |
| Password con sale e 20 000 passaggi di SHA-256, blocco automatico | ✅ |
| Richieste di permesso (fotocamera, microfono, cartelle) | ✅ |

**Aggiornamenti atomici A/B.** Fatto (vedi
[architettura](architettura.md#aggiornamenti-del-sistema-ab)): il kernel e
il pacchetto di avvio con tutti i driver del sistema stanno in due slot
nella partizione EFI (`ARCOS/SLOTA.BIN`, `SLOTB.BIN`), `ARCOS/BOOT.CTL`
dice quale avviare.

1. L'aggiornamento arriva dall'indice firmato (`make updates` vi mette
   l'immagine di slot) e si scrive nello slot inattivo. Il sistema lo
   rilegge e ne verifica lo SHA-256.
2. Lo slot nuovo diventa attivo con due tentativi, che ArcBoot (BIOS e
   UEFI) consuma uno per avvio.
3. Il sistema nuovo, se gira per 15 secondi, si dichiara buono. Un panic
   durante la prova riavvia la macchina; con i tentativi finiti ArcBoot
   torna all'altro slot, che lo segnala.

Info sistema installa, chiede il riavvio, annulla e torna alla versione
precedente; dal terminale c'è `sysupdate`. Restano da fare:

- l'immagine delta, per scaricare solo le differenze;
- i file di sistema di `sys:` (`rootfs`) dentro lo slot (oggi si aggiornano
  le app e il kernel con i driver, non i file come sfondi e font);
- gli snapshot del file system, che richiedono il copy-on-write in ArcFS.

**Permessi.** Fatto (vedi «Identità delle app e sandbox»): quando un'app
apre una cartella fuori dalla sua, il kernel sospende la chiamata e mostra
«*Disegno* vuole usare la cartella Immagini — Consenti / Non consentire»;
la risposta resta in `sys:/system/permissions.json`, il servizio
`arc.permissions` risponde per fotocamera, microfono, posizione e rete,
*Impostazioni → Privacy delle app* elenca, cambia e revoca; il Negozio
mostra i permessi dichiarati nel manifest di ogni app.

## Ordine consigliato

| Fase | Contenuto | Durata indicativa |
|---|---|---|
| 1 | ✅ IPC a porte, identità delle app, desktop virtuali, schede in File | fatto |
| 2 | ✅ sandbox, permessi, aggiornamenti delle app, trascinamento | fatto |
| 3 | ✅ interprete AML, batteria, temperatura, sospensione | fatto |
| 4 | ✅ aggiornamenti A/B; HiDPI, indice dei file | 2–3 mesi |
| 5 | strato Linux completo (JavaScript in `arcweb`: fatto), poi Servo | 6–12 mesi |
| 6 | Wi-Fi (un chip), Bluetooth HID e audio | 6–9 mesi |
| 7 | accelerazione grafica (virgl, poi hardware vero) | 12 mesi e oltre |

Ognuna è utilizzabile appena finita: niente aspetta l'ultima.
