# hello.as — the basics of ArcScript

let name = "JustOS"
print("Hello, {name}!")

fn fib(n)
    if n < 2
        return n
    return fib(n - 1) + fib(n - 2)

let squares = []
for i in 1..8
    squares.push(i * i)

print("squares: {squares}")
print("fib(25) = {fib(25)}")

let total = 0
for x in squares
    if x % 2 == 0
        continue
    total += x
print("sum of odd squares = {total}")
