package fixture.app

import fixture.gen.Version
import fixture.greeting.Greeter
import fixture.greeting.Strings
import fixture.shapes.Circle
import fixture.shapes.Shape
import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class Report(@SerialName("label") val name: String, @EncodeDefault val area: Double = 0.0)

val reportDescriptor = Report.serializer().descriptor

internal fun appInternal(): String = helper(Strings.shout(Greeter().greet(BuildInfo.NAME)))

fun describe(shape: Shape): Report = Report("${Version.VALUE}: ${shape.name}", shape.area())

fun main() {
    println(appInternal())
    println(describe(Circle(1.0)))
}
