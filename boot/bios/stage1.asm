; ArcBoot stage 1 — master boot record, and boot sector of the ISO.
;
; The BIOS loads this sector at 0000:7C00. It reads stage 2 (from LBA 1 on a
; disk) to 0000:7E00 with the INT 13h extensions and jumps there with the
; boot drive in DL. The boot table at offset 0x180 is filled in by `arctool`
; when the image is built; stage 2 reads it from memory.
;
; On a CD (El Torito, no emulation) sectors are 2048 bytes: the boot table
; says so, and gives every position in 512-byte units, multiples of four.

bits 16
org 0x7C00

STAGE2_ADDR     equ 0x7E00
BOOT_CD         equ 1               ; boot_flags: 2048-byte sectors

start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7C00
    jmp 0x0000:.flush               ; normalise CS:IP to 0000:7Cxx
.flush:
    sti
    cld
    mov [boot_drive], dl

    cmp dword [boot_magic], 0x42435241   ; "ARCB"
    jne fail_table

    ; INT 13h extensions are required (every BIOS since the late 90s).
    mov ah, 0x41
    mov bx, 0x55AA
    mov dl, [boot_drive]
    int 0x13
    jc fail_disk
    cmp bx, 0xAA55
    jne fail_disk

    ; Read stage 2 (boot_stage2_sectors sectors at boot_stage2_lba) to
    ; 0000:7E00, in 2048-byte sectors on a CD.
    mov ax, [boot_stage2_sectors]
    mov ebx, [boot_stage2_lba]
    test byte [boot_flags], BOOT_CD
    jz .units_ok
    add ax, 3
    shr ax, 2
    shr ebx, 2
.units_ok:
    mov [dap_count], ax
    mov word [dap_offset], STAGE2_ADDR
    mov word [dap_segment], 0
    mov [dap_lba], ebx
    mov dword [dap_lba + 4], 0
    mov si, dap
    mov ah, 0x42
    mov dl, [boot_drive]
    int 0x13
    jc fail_disk

    mov dl, [boot_drive]
    jmp 0x0000:STAGE2_ADDR

fail_table:
    mov si, msg_table
    jmp halt
fail_disk:
    mov si, msg_disk
halt:
    call print
.hang:
    cli
    hlt
    jmp .hang

; print: SI = zero-terminated string
print:
    lodsb
    test al, al
    jz .done
    mov ah, 0x0E
    mov bx, 0x0007
    int 0x10
    jmp print
.done:
    ret

msg_disk    db "disk error", 0
msg_table   db "no boot table", 0

boot_drive  db 0

align 4
dap:
    db 16, 0
dap_count   dw 0
dap_offset  dw 0
dap_segment dw 0
dap_lba     dq 0

; ---------------------------------------------------------------------------
; Boot table (written by arctool). Layout shared with stage2.asm. Positions
; and sizes count 512-byte sectors.
times 0x180 - ($ - $$) db 0
boot_stage2_lba     dd 0
boot_system_lba     dd 0            ; a live system volume to load, if any
boot_system_sectors dd 0
boot_ctl_lba        dd 0            ; ARCOS/BOOT.CTL: picks the system slot
boot_magic          dd 0            ; "ARCB"
boot_stage2_sectors dw 0
boot_flags          dw 0            ; BOOT_CD
boot_kernel_lba     dd 0
boot_kernel_sectors dd 0
boot_kernel_memsz   dd 0            ; bytes, including .bss
boot_pack_lba       dd 0
boot_pack_sectors   dd 0
boot_kernel_entry   dq 0            ; virtual address of the kernel entry
; 0x1B4: end of the boot table. 0x1B8: disk signature, 0x1BE: partitions.
times 0x1FE - ($ - $$) db 0
dw 0xAA55
