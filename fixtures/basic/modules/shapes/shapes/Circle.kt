package fixture.shapes

class Circle(private val radius: Double) : Shape {
    override val name: String = "circle"

    override fun area(): Double = Math.PI * radius * radius
}

internal fun unitCircle(): Circle = Circle(1.0)
