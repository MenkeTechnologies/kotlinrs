// Recursive Fibonacci — exercises user functions, recursion, and if-expressions.
fun fib(n: Int): Int {
    return if (n < 2) n else fib(n - 1) + fib(n - 2)
}

fun main() {
    for (i in 0..10) {
        println("fib($i) = ${fib(i)}")
    }
}
