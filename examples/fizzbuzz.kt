// FizzBuzz — integer modulo, string concatenation, and control flow.
fun main() {
    for (n in 1..15) {
        val fizz = n % 3 == 0
        val buzz = n % 5 == 0
        if (fizz && buzz) {
            println("FizzBuzz")
        } else if (fizz) {
            println("Fizz")
        } else if (buzz) {
            println("Buzz")
        } else {
            println(n)
        }
    }
}
