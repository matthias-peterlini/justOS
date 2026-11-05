JustOS: un sistema operativo per PC x86-64 scritto da zero, con il linguaggio ArcScript.

![La scrivania di JustOS](https://raw.githubusercontent.com/matthias-peterlini/justOS/main/docs/screenshots/scrivania.jpg)

## Novità

- **Il nuovo stile arc**: barra in alto flottante in vetro chiaro (logo, ora e data al centro, stato a destra), finestre con il titolo integrato e i tre pulsanti tondi, dock flottante, centro di controllo con riquadri, cursori di volume e luminosità e scheda dell'account, centro notifiche con il calendario; a schermo intero la barra si aggancia e mostra il nome dell'app e i pulsanti della finestra. Il carattere è Google Sans Flex.
- **Doppio clic per installare**: pacchetti Debian `.deb`, archivi `.tar.gz`, `.tgz`, `.tar`, `.zip` e `.gz` con un programma Linux statico o Windows x64 si installano da soli; i pacchetti macOS `.pkg` vengono scompattati (i programmi macOS non possono girare su JustOS). Ogni altro programma (ELF o `.exe`) parte con un doppio clic.

## Novità della 0.6

- **L'ecosistema ticly**: ticly Do (panoramica, attività, note, calendario per mese, settimana e giorno, profilo), ticly AI (l'interfaccia di ai.ticly.it con le chat salvate; **in anteprima: le risposte non sono ancora collegate**) e il **ticly browser**, con un unico account ticly. Per ora l'account e i dati restano sul computer: nessuna di queste app usa servizi esterni.
- **Il ticly browser** con un motore web scritto da zero (`justweb`): HTML, CSS con flexbox, grid, tabelle e media query, immagini PNG, JPEG e GIF; schede, cronologia, ricerca DuckDuckGo, pagina iniziale ticly, trova nella pagina, moduli, cookie e download. JavaScript non c'è.
- **Rete vera**: TCP, TLS 1.3 con certificati verificati, HTTP e HTTPS.
- **Il Negozio scarica app vere**, con la ricerca: programmi Linux e Windows oltre alle app di JustOS.
- **Lo stile arc**: il logo ARC, pannelli ticly in vetro con bagliore ruggine, nuovi sfondi; le app a schermo intero (F11) non hanno barre in alto.
- **Più compatibilità Linux**: thread, futex, mmap, poll e socket.

![Il Negozio](https://raw.githubusercontent.com/matthias-peterlini/justOS/main/docs/screenshots/negozio.jpg)

Ancora senza driver: Wi-Fi, schede video vere (Intel, AMD, nVidia: si usa il framebuffer del firmware), batteria dei portatili. Lo stato di ogni driver è in [docs/driver.md](https://github.com/matthias-peterlini/justOS/blob/main/docs/driver.md#hardware-supportato).

## File

| File | Cosa farne |
|---|---|
| `justos.iso` | **CD live**: si avvia in una macchina virtuale (VirtualBox, VMware, QEMU, Hyper-V di prima generazione) o da un CD, con BIOS o UEFI, senza disco rigido. Il sistema gira in memoria: le modifiche si perdono allo spegnimento. |
| `justos-img.zip` | **Immagine di disco** (128 MiB, BIOS e UEFI): scompattata, si scrive su una chiavetta USB (balenaEtcher, Rufus in modalità DD, `dd`) per avviare un PC vero, con le modifiche che restano. Secure Boot va disattivato. |
| `SHA256SUMS` | le impronte dei due file |

## Provarlo

- **QEMU**: `qemu-system-x86_64 -m 512M -cdrom justos.iso -device ich9-intel-hda -device hda-output`
- **VirtualBox**: nuova macchina "Other/Unknown (64-bit)", 512 MiB di memoria, nessun disco, `justos.iso` nel lettore CD.

Al primo avvio JustOS chiede nome, tastiera e sfondo, poi apre la scrivania. Il Terminale (nel dock) ha la shell: `help` elenca i comandi. Documentazione nel [README](https://github.com/matthias-peterlini/justOS#readme).
