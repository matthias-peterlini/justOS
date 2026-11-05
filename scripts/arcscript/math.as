# Checks JustOS's own f64 functions (arcc-core/src/float.rs) against the
# values of the C library; prints how many agree.
import math

let checks = [
    ["math.sqrt(2)", math.sqrt(2), 1.4142135623730951],
    ["math.sqrt(1e-300)", math.sqrt(1e-300), 1e-150],
    ["math.sin(1)", math.sin(1), 0.8414709848078965],
    ["math.cos(1)", math.cos(1), 0.5403023058681398],
    ["math.tan(0.5)", math.tan(0.5), 0.5463024898437905],
    ["math.sin(100)", math.sin(100), -0.5063656411097588],
    ["math.cos(-3)", math.cos(-3), -0.9899924966004454],
    ["math.asin(0.3)", math.asin(0.3), 0.3046926540153975],
    ["math.acos(-0.7)", math.acos(-0.7), 2.3461938234056494],
    ["math.atan(5)", math.atan(5), 1.373400766945016],
    ["math.atan2(-1, -2)", math.atan2(-1, -2), -2.677945044588987],
    ["math.exp(1)", math.exp(1), 2.718281828459045],
    ["math.exp(-20.5)", math.exp(-20.5), 1.2501528663867426e-09],
    ["math.log(10)", math.log(10), 2.302585092994046],
    ["math.log(8, 2)", math.log(8, 2), 3.0],
    ["math.log10(1000)", math.log10(1000), 3.0],
    ["math.log10(2)", math.log10(2), 0.3010299956639812],
    ["math.log2(10)", math.log2(10), 3.321928094887362],
    ["math.pow(2.0, 0.5)", math.pow(2.0, 0.5), 1.4142135623730951],
    ["math.pow(1.5, 2.7)", math.pow(1.5, 2.7), 2.988452789872502],
    ["math.pow(10.0, -2)", math.pow(10.0, -2), 0.01],
    ["math.hypot(3, 4)", math.hypot(3, 4), 5.0],
    ["math.pi * 2", math.pi * 2, 6.283185307179586],
    ["math.floor(-2.5)", math.floor(-2.5), -3],
    ["math.round(2.5)", math.round(2.5), 3],
    ["math.round(3.14159, 2)", math.round(3.14159, 2), 3.14],
]
let bad = 0
for c in checks
    let want = c[2]
    if math.abs(c[1] - want) > 4e-16 * max(1, math.abs(want))
        print("math: {c[0]} = {c[1]}, expected {want}")
        bad += 1
print("math: {len(checks) - bad} of {len(checks)} agree")
print(1.0 / 3, 0.1 + 0.2, 2.5 * 4, 7 / 2)
