/*
 * A laptop for QEMU: a battery, a power adapter, a lid and a thermal zone,
 * added to the virtual machine's ACPI tables with
 *     -acpitable file=scripts/acpi/laptop.aml
 * scripts/test-acpi.sh checks that justOS reads them, and that it turns
 * itself off when the battery runs empty.
 *
 * Build: iasl -p laptop laptop.asl
 */
DefinitionBlock ("", "SSDT", 2, "JUSTOS", "LAPTOP", 1)
{
    Scope (\_SB)
    {
        Device (AC0)
        {
            Name (_HID, "ACPI0003")
            Name (ONAC, One)
            Method (_PSR, 0, NotSerialized)
            {
                Return (ONAC)
            }
        }

        Device (BAT0)
        {
            Name (_HID, EisaId ("PNP0C0A"))
            Name (_UID, One)
            /* Remaining capacity, mWh. */
            Name (REMC, 40000)
            Name (BSTP, Package (0x04) { Zero, 9000, Zero, 11800 })

            Method (_STA, 0, NotSerialized)
            {
                Return (0x1F)
            }

            Method (_BIX, 0, NotSerialized)
            {
                /* Literal numbers: iasl 20251212 rejects Zero in a _BIX. */
                Return (Package (0x15)
                {
                    0x01,           /* revision */
                    0x00,           /* power unit: mW and mWh */
                    57000,          /* design capacity */
                    50000,          /* last full charge */
                    One,            /* rechargeable */
                    11400,          /* design voltage, mV */
                    5000,           /* warning capacity */
                    2500,           /* low capacity */
                    123,            /* cycle count */
                    100000,         /* measurement accuracy */
                    0xFFFFFFFF,     /* max sampling time */
                    0xFFFFFFFF,     /* min sampling time */
                    0xFFFFFFFF,     /* max averaging interval */
                    0xFFFFFFFF,     /* min averaging interval */
                    100,            /* granularity 1 */
                    100,            /* granularity 2 */
                    "JST57",        /* model */
                    "0001",         /* serial number */
                    "LION",         /* type */
                    "Just Cells",   /* OEM */
                    0x00            /* swapping capability */
                })
            }

            Method (_BST, 0, Serialized)
            {
                If (\_SB.AC0.ONAC)
                {
                    BSTP [Zero] = 0x02  /* charging */
                }
                Else
                {
                    BSTP [Zero] = One   /* discharging */
                }

                BSTP [0x02] = REMC
                Return (BSTP)
            }

            /* For tests: sets the charge and the adapter, as an EC would. */
            Method (SETC, 2, NotSerialized)
            {
                REMC = Arg0
                \_SB.AC0.ONAC = Arg1
                Notify (\_SB.AC0, 0x80)
                Notify (BAT0, 0x80)
            }
        }

        Device (LID0)
        {
            Name (_HID, EisaId ("PNP0C0D"))
            Name (LIDV, One)
            Method (_LID, 0, NotSerialized)
            {
                Return (LIDV)
            }
        }
    }

    Scope (\_TZ)
    {
        ThermalZone (TZ01)
        {
            Name (TMPV, 3132)
            Method (_TMP, 0, NotSerialized)
            {
                Return (TMPV)
            }

            Method (_CRT, 0, NotSerialized)
            {
                Return (3732)
            }
        }
    }
}
