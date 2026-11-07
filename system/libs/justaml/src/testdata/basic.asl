/*
 * Tests of the justaml interpreter (src/tests.rs). Each Tnn method returns a
 * bit mask of the checks that passed; the Rust test knows the full mask.
 *
 * Rebuild with: iasl -p basic basic.asl
 */
DefinitionBlock ("", "DSDT", 2, "JUSTOS", "AMLTEST", 1)
{
    External (\_SB.MISS, DeviceObj)

    Name (GINT, 0x10)
    Name (GSTR, "hello")
    Name (GBUF, Buffer (4) { 1, 2, 3, 4 })
    Name (GPKG, Package (3) { 1, "two", Buffer (2) { 3, 3 } })
    Name (_S3, Package () { 1, 1, 0, 0 })
    Name (_S5, Package (4) { 5, 5, 0, 0 })

    /* Code outside methods runs while the table loads. */
    Name (MLC, 0)
    If (CondRefOf (\_OSI)) {
        MLC = 1
    }

    /* Arithmetic */
    Method (T01, 0)
    {
        Local7 = 0
        Local0 = 5 + 7
        Local1 = Local0 * 3
        Local2 = Local1 - 6
        Divide (Local2, 4, Local4, Local5)
        Local6 = (1 << 4) | 3
        If (Local0 == 12) { Local7 |= 1 }
        If (Local2 / 4 == 7) { Local7 |= 2 }
        If ((Local4 == 2) && (Local5 == 7)) { Local7 |= 4 }
        If (Local6 == 0x13) { Local7 |= 8 }
        If ((~Local0 & 0xFF) == 0xF3) { Local7 |= 0x10 }
        If (Local2 % 7 == 2) { Local7 |= 0x20 }
        Local0 = 0xFFFFFFFFFFFFFFFF
        Local0 += 2
        If (Local0 == 1) { Local7 |= 0x40 }
        Local1 = 0
        Local1--
        If (Local1 == Ones) { Local7 |= 0x80 }
        If (0x8000000000000000 >> 63 == 1) { Local7 |= 0x100 }
        If ((0xF0 ^ 0xFF) == 0x0F) { Local7 |= 0x200 }
        If (NAnd (0xFF, 0x0F) == 0xFFFFFFFFFFFFFFF0) { Local7 |= 0x400 }
        Return (Local7)
    }

    /* Loops */
    Method (T02, 0)
    {
        Local7 = 0
        Local0 = 0
        For (Local1 = 0, Local1 < 10, Local1++) {
            Local0 += Local1
        }
        If (Local0 == 45) { Local7 |= 1 }
        Local2 = 0
        While (One) {
            Local2++
            If (Local2 >= 5) { Break }
        }
        If (Local2 == 5) { Local7 |= 2 }
        Local3 = 0
        Local4 = 0
        While (Local3 < 10) {
            Local3++
            If (Local3 & 1) { Continue }
            Local4++
        }
        If (Local4 == 5) { Local7 |= 4 }
        Return (Local7)
    }

    /* Strings and buffers */
    Method (T03, 0)
    {
        Local7 = 0
        Local0 = Concatenate ("ab", "cd")
        If (Local0 == "abcd") { Local7 |= 1 }
        If (SizeOf (Local0) == 4) { Local7 |= 2 }
        If (ToHexString (GINT) == "0x10") { Local7 |= 4 }
        If (ToDecimalString (GINT) == "16") { Local7 |= 8 }
        If (Mid ("hello world", 6, 5) == "world") { Local7 |= 0x10 }
        Local1 = "0x10"
        Local2 = "10"
        If (ToInteger (Local1) + ToInteger (Local2) == 26) { Local7 |= 0x20 }
        Local3 = ToBuffer (GSTR)
        If (SizeOf (Local3) == 6) { Local7 |= 0x40 }
        If (Concatenate (GSTR, 1) == "hello0000000000000001") { Local7 |= 0x80 }
        Local5 = Concatenate (GBUF, Buffer () { 9, 9 })
        If (SizeOf (Local5) == 6) { Local7 |= 0x100 }
        Local4 = Buffer () { 0x41, 0x42, 0, 0x43 }
        If (ToString (Local4, Ones) == "AB") { Local7 |= 0x200 }
        If ("abc" < "abd") { Local7 |= 0x400 }
        If (GSTR != "hellp") { Local7 |= 0x800 }
        If (GBUF == Buffer () { 1, 2, 3, 4 }) { Local7 |= 0x1000 }
        Return (Local7)
    }

    /* Packages and Index */
    Method (T04, 0)
    {
        Local7 = 0
        Local0 = Package (4) { 1, 2, 3 }
        Local0[3] = 40
        If (DerefOf (Local0[1]) + DerefOf (Local0[3]) == 42) { Local7 |= 1 }
        GPKG[0] = 7
        If (DerefOf (GPKG[0]) == 7) { Local7 |= 2 }
        If (DerefOf (GPKG[1]) == "two") { Local7 |= 4 }
        If (SizeOf (GPKG) == 3) { Local7 |= 8 }
        GBUF[2] = 0x33
        If (DerefOf (GBUF[2]) == 0x33) { Local7 |= 0x10 }
        Local1 = DerefOf (GPKG[2])
        If (DerefOf (Local1[1]) == 3) { Local7 |= 0x20 }
        Store (Index (GPKG, 1), Local2)
        If (DerefOf (Local2) == "two") { Local7 |= 0x40 }
        If (SizeOf (Local0) == 4) { Local7 |= 0x80 }
        Return (Local7)
    }

    /* Buffer fields */
    Method (T05, 0)
    {
        Local7 = 0
        Local0 = Buffer (8) { 0x78, 0x56, 0x34, 0x12, 0xFF, 0, 0, 0 }
        CreateDWordField (Local0, 0, DW0)
        CreateByteField (Local0, 4, BY4)
        CreateBitField (Local0, 33, BT33)
        CreateField (Local0, 4, 8, NIB)
        CreateQWordField (Local0, 0, QW0)
        If (DW0 == 0x12345678) { Local7 |= 1 }
        If (BY4 == 0xFF) { Local7 |= 2 }
        BY4 = 0x0F
        If (BT33 == 1) { Local7 |= 4 }
        If (NIB == Buffer () { 0x67 }) { Local7 |= 8 }
        DW0 = 0xAABBCCDD
        If (DerefOf (Local0[0]) == 0xDD) { Local7 |= 0x10 }
        If (QW0 == 0x0FAABBCCDD) { Local7 |= 0x20 }
        CreateWordField (GBUF, 0, GW0)
        GW0 = 0xBEEF
        If (DerefOf (GBUF[1]) == 0xBE) { Local7 |= 0x40 }
        Return (Local7)
    }

    /* Switch: becomes a While with a method-local name */
    Method (T06, 1)
    {
        Switch (ToInteger (Arg0)) {
            Case (1) {
                Return ("one")
            }
            Case (Package () { 2, 3 }) {
                Return ("two-three")
            }
            Default {
                Return ("other")
            }
        }
    }

    /* _OSI */
    Method (T07, 0)
    {
        Local7 = 0
        If (_OSI ("Windows 2015")) { Local7 |= 1 }
        If (!_OSI ("Some Other OS")) { Local7 |= 2 }
        If (CondRefOf (\_OSI, Local0)) { If (ObjectType (Local0) == 8) { Local7 |= 4 } }
        If (MLC == 1) { Local7 |= 8 }
        Return (Local7)
    }

    /* Memory fields */
    OperationRegion (MEM0, SystemMemory, 0x1000, 0x100)
    Field (MEM0, AnyAcc, NoLock, Preserve)
    {
        MB00,   8,
        MB01,   8,
        MW02,   16,
        MD04,   32,
        Offset (0x10),
            ,   3,
        MBT3,   2,
        Offset (0x20),
        MQ20,   64,
        Offset (0x30),
        MBIG,   128
    }
    Field (MEM0, ByteAcc, NoLock, WriteAsOnes)
    {
        Offset (0x40),
            ,   4,
        WAO,    4
    }

    Method (T08, 0)
    {
        MD04 = 0x11223344
        MB00 = 0xAB
        MBT3 = 3
        MQ20 = 0x0102030405060708
        WAO = 5
        Return (Package () { MB00, MD04, MQ20, MBIG, MW02 })
    }

    /* CMOS through an index field */
    OperationRegion (CMS1, SystemIO, 0x70, 2)
    Field (CMS1, ByteAcc, NoLock, Preserve)
    {
        CIDX,   8,
        CDAT,   8
    }
    IndexField (CIDX, CDAT, ByteAcc, NoLock, Preserve)
    {
        Offset (0x10),
        CM10,   8,
        CM11,   8,
            ,   4,
        CM12,   4
    }

    Method (T09, 0)
    {
        CM11 = 0x5A
        CM12 = 0x0C
        Return (CM10 + (CM11 << 8))
    }

    /* Bank fields */
    OperationRegion (BNK0, SystemIO, 0x200, 4)
    Field (BNK0, ByteAcc, NoLock, Preserve)
    {
        BSEL,   8
    }
    BankField (BNK0, BSEL, 2, ByteAcc, NoLock, Preserve)
    {
        Offset (1),
        BKF2,   8
    }

    Method (T10, 0)
    {
        BKF2 = 0x44
        Return (BKF2)
    }

    /* Devices */
    Name (INIC, 0)
    Scope (\_SB)
    {
        Method (_INI, 0)
        {
            INIC += 0x100
        }

        Device (PCI0)
        {
            Name (_HID, EisaId ("PNP0A08"))
            Name (_CID, EisaId ("PNP0A03"))
            Name (_BBN, 0)
            Device (BRG1)
            {
                Name (_ADR, 0x001C0000)
                Device (DEV0)
                {
                    Name (_ADR, 0x02)
                    OperationRegion (PCFG, PCI_Config, 0, 0x100)
                    Field (PCFG, DWordAcc, NoLock, Preserve)
                    {
                        VDID,   32
                    }
                }
            }
        }

        Device (DEVA)
        {
            Name (_HID, "JST0001")
            Method (_STA, 0) { Return (0x0F) }
            Method (_INI, 0) { INIC += 1 }
            Device (DEVB)
            {
                Name (_HID, EisaId ("PNP0C0A"))
                Method (_INI, 0) { INIC += 0x10 }
            }
            Method (UPS, 0) { Return (MLC) }
        }

        Device (DEVC)
        {
            Name (_HID, "JST0002")
            Method (_STA, 0) { Return (0) }
            Method (_INI, 0) { INIC += 0x1000 }
            Device (DEVD)
            {
                Name (_ADR, 1)
                Method (_INI, 0) { INIC += 0x10000 }
            }
        }

        Device (DEVE)
        {
            Name (_HID, "JST0003")
            Method (_STA, 0) { Return (0x08) }
            Device (DEVF)
            {
                Name (_ADR, 2)
                Method (_INI, 0) { INIC += 0x100000 }
            }
        }

        Name (PKGN, Package () { DEVA, GINT, 3 })

        Device (EC0)
        {
            Name (_HID, EisaId ("PNP0C09"))
            Name (_CRS, ResourceTemplate ()
            {
                IO (Decode16, 0x0062, 0x0062, 0x00, 0x01)
                IO (Decode16, 0x0066, 0x0066, 0x00, 0x01)
            })
            Name (_GPE, 0x17)
            Name (REGC, 0)
            Method (_REG, 2)
            {
                If (Arg0 == 3) { REGC = Arg1 }
            }
            OperationRegion (ERAM, EmbeddedControl, 0, 0xFF)
            Field (ERAM, ByteAcc, Lock, Preserve)
            {
                Offset (0x10),
                BTST,   8,
                BCAP,   16
            }
            Method (_Q20, 0)
            {
                Notify (BAT0, 0x80)
            }
            Device (BAT0)
            {
                Name (_HID, EisaId ("PNP0C0A"))
                Name (_UID, 1)
                Method (_STA, 0) { Return (0x1F) }
                Method (_BST, 0)
                {
                    Local0 = Package (4) { 0, 0, 0, 0 }
                    Local0[0] = BTST
                    Local0[1] = 1000
                    Local0[2] = BCAP
                    Local0[3] = 12000
                    Return (Local0)
                }
            }
        }
    }

    Scope (\_TZ)
    {
        ThermalZone (TZ00)
        {
            Method (_TMP, 0) { Return (3132) }
            Method (_CRT, 0) { Return (3732) }
        }
    }

    PowerResource (PWR0, 0, 0)
    {
        Method (_STA, 0) { Return (1) }
        Method (_ON, 0) { }
        Method (_OFF, 0) { }
    }

    Method (T11, 0)
    {
        Notify (\_SB.DEVA.DEVB, 0x80)
        Return (1)
    }

    /* Method-local names, recursion */
    Method (MLN, 0)
    {
        Name (LOCN, 5)
        Return (LOCN)
    }
    Method (FACT, 1)
    {
        If (Arg0 <= 1) { Return (1) }
        Return (Arg0 * FACT (Arg0 - 1))
    }
    Method (T12, 0)
    {
        Local0 = MLN ()
        Local1 = MLN ()
        Return (Local0 + Local1 + FACT (5))
    }

    /* A named package passed to a method is changed by it */
    Name (PKGA, Package (2) { 0, 0 })
    Method (SETP, 1)
    {
        Arg0[1] = 0x55
    }
    Method (T13, 0)
    {
        SETP (PKGA)
        Return (DerefOf (PKGA[1]))
    }

    /* References */
    Name (RVAL, 0)
    Method (SETR, 1)
    {
        Arg0 = 0x77
    }
    Method (T14, 0)
    {
        SETR (RefOf (RVAL))
        Local0 = RefOf (RVAL)
        Local1 = DerefOf (Local0)
        Return (RVAL + Local1)
    }

    /* Odd operators */
    Method (T15, 0)
    {
        Local7 = 0
        If (FindSetLeftBit (0x80) == 8) { Local7 |= 1 }
        If (FindSetRightBit (0x80) == 8) { Local7 |= 2 }
        If (FromBCD (0x1234) == 1234) { Local7 |= 4 }
        If (ToBCD (1234) == 0x1234) { Local7 |= 8 }
        If (Match (Package () { 1, 5, 9 }, MGT, 4, MLT, 9, 0) == 1) { Local7 |= 0x10 }
        If (ObjectType (GSTR) == 2) { Local7 |= 0x20 }
        If (ObjectType (\_SB.DEVA) == 6) { Local7 |= 0x40 }
        If (!CondRefOf (\_SB.MISS)) { Local7 |= 0x80 }
        If (ObjectType (FACT) == 8) { Local7 |= 0x100 }
        If (Match (Package () { 1, 5, 9 }, MEQ, 7, MTR, 0, 0) == Ones) { Local7 |= 0x200 }
        Return (Local7)
    }

    /* Mutexes, events, time */
    Mutex (MTX0, 0)
    Event (EVT0)
    Method (T16, 0)
    {
        Local7 = 0
        If (Acquire (MTX0, 0xFFFF) == 0) { Local7 |= 1 }
        Release (MTX0)
        Signal (EVT0)
        If (Wait (EVT0, 0) == 0) { Local7 |= 2 }
        If (Wait (EVT0, 0) != 0) { Local7 |= 4 }
        Sleep (1)
        Stall (10)
        Local0 = Timer
        Local1 = Timer
        If (Local1 > Local0) { Local7 |= 8 }
        Debug = "dbg"
        Return (Local7)
    }

    /* Stores convert to the type of a named object */
    Name (TINT, 0)
    Name (TSTR, "")
    Name (TBUF, Buffer (2) { })
    Alias (TINT, TALS)
    Method (T17, 0)
    {
        Local7 = 0
        TINT = "1A"
        If (TINT == 0x1A) { Local7 |= 1 }
        TSTR = 0x2B
        If (TSTR == "000000000000002B") { Local7 |= 2 }
        TBUF = 0x030201
        If ((SizeOf (TBUF) == 2) && (DerefOf (TBUF[1]) == 2)) { Local7 |= 4 }
        TALS = 0x99
        If (TINT == 0x99) { Local7 |= 8 }
        CopyObject ("now a string", TINT)
        If (ObjectType (TINT) == 2) { Local7 |= 0x10 }
        Return (Local7)
    }

    /* Errors */
    Method (TINF, 0)
    {
        While (One) { Noop }
    }
    Method (RECU, 1)
    {
        Return (RECU (Arg0 + 1))
    }
    Method (TDIV, 1)
    {
        Return (10 / Arg0)
    }
    Method (TIDX, 0)
    {
        Return (DerefOf (GPKG[5]))
    }
}
