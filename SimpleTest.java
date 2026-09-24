public class SimpleTest {
    public static void main(String[] args) {
        StringBuilder sb = new StringBuilder();
        sb.append("test:");
        for (int i = 0; i < 5; i++) {
            if (i % 2 == 0) {
                sb.append("even").append(i);
            } else {
                sb.append("odd").append(i);
            }
        }
        System.out.println(sb.toString());
    }
}