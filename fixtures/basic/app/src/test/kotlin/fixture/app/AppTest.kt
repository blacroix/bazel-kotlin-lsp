package fixture.app

import org.junit.Assert.assertEquals
import org.junit.Test

class AppTest {
    @Test
    fun greets() {
        assertEquals("[HELLO, FIXTURE]", appInternal())
    }
}
