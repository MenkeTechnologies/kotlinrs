// while, for/until/downTo/step, mutation, and mixed numeric/string output.
fun main() {
    var sum = 0
    for (i in 1..100) {
        sum += i
    }
    println("sum 1..100 = $sum")

    var i = 3
    while (i > 0) {
        println("countdown $i")
        i -= 1
    }

    for (k in 0 until 6 step 2) {
        print("$k ")
    }
    println("")

    for (j in 5 downTo 1) {
        print("$j ")
    }
    println("")

    val pi = 3.0 + 0.14
    println("pi ~ $pi, half = ${pi / 2.0}, seven-halves = ${7 / 2}")
}
