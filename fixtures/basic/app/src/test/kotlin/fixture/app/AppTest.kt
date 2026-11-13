package fixture.app

import fixture.greeting.Greeter
import kotlinx.serialization.SerialName
import org.junit.Assert.assertEquals
import org.junit.Test

class AppTest {
    @Test
    fun greets() {
        assertEquals("[HELLO, FIXTURE]", appInternal())
    }

    @Test
    fun seesTransitiveDependencies() {
        assertEquals("Hello, x", Greeter().greet("x"))
        assertEquals("SerialName", SerialName::class.simpleName)
    }
}
