/*
 * A revision 1 table: integers are 32 bits wide (src/tests.rs).
 *
 * Rebuild with: iasl -p narrow narrow.asl
 */
DefinitionBlock ("", "DSDT", 1, "ARCOS", "AML32", 1)
{
    Method (T32, 0)
    {
        Local7 = 0
        Local0 = 0xFFFFFFFF
        Local0++
        If (Local0 == 0) { Local7 |= 1 }
        Local1 = Ones
        If (Local1 == 0xFFFFFFFF) { Local7 |= 2 }
        Local3 = ToBuffer (Local1)
        If (SizeOf (Local3) == 4) { Local7 |= 4 }
        Local2 = Buffer () { 1, 2, 3, 4, 5, 6, 7, 8 }
        CreateField (Local2, 0, 64, QQ)
        If (ObjectType (QQ) == 14) { Local7 |= 8 }
        Return (Local7)
    }
}
