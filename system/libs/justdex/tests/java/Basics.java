import java.util.*;

/** A tour of the Java the interpreter has to run: its output is compared
 *  with what a real JVM prints. */
public class Basics {
    interface Shape {
        double area();
        String name();
    }

    static abstract class Base implements Shape, Comparable<Base> {
        static int made = 0;
        final String label;

        Base(String label) {
            this.label = label;
            made++;
        }

        public String name() {
            return label;
        }

        public int compareTo(Base o) {
            return Double.compare(area(), o.area());
        }

        @Override
        public String toString() {
            return String.format("%s(%.2f)", name(), area());
        }
    }

    static class Circle extends Base {
        final double r;

        Circle(double r) {
            super("cerchio");
            this.r = r;
        }

        public double area() {
            return Math.PI * r * r;
        }
    }

    static class Rect extends Base {
        final int w, h;

        Rect(int w, int h) {
            super("rettangolo");
            this.w = w;
            this.h = h;
        }

        public double area() {
            return w * h;
        }

        @Override
        public String name() {
            return w == h ? "quadrato" : super.name();
        }
    }

    enum Day { LUN, MAR, MER }

    static class Point {
        final int x, y;

        Point(int x, int y) {
            this.x = x;
            this.y = y;
        }

        @Override
        public boolean equals(Object o) {
            if (!(o instanceof Point)) return false;
            Point p = (Point) o;
            return p.x == x && p.y == y;
        }

        @Override
        public int hashCode() {
            return 31 * x + y;
        }

        @Override
        public String toString() {
            return "(" + x + "," + y + ")";
        }
    }

    static final String GREETING = "Ciao";
    static int[] squares = new int[5];

    static {
        for (int i = 0; i < squares.length; i++) squares[i] = i * i;
    }

    static int fib(int n) {
        return n < 2 ? n : fib(n - 1) + fib(n - 2);
    }

    static long fact(int n) {
        long r = 1;
        for (int i = 2; i <= n; i++) r *= i;
        return r;
    }

    static String kind(String s) {
        switch (s) {
            case "mela":
            case "pera":
                return "frutta";
            case "carota":
                return "verdura";
            default:
                return "boh";
        }
    }

    static String day(Day d) {
        switch (d) {
            case LUN: return "inizio";
            case MER: return "metà";
            default: return "altro";
        }
    }

    static int divide(int a, int b) {
        try {
            return a / b;
        } catch (ArithmeticException e) {
            System.out.println("errore: " + e.getMessage());
            return -1;
        } finally {
            System.out.println("finally " + a + "/" + b);
        }
    }

    public static void main(String[] args) {
        System.out.println(GREETING + " da Java! args=" + args.length);

        // Numbers.
        int a = 17, b = 5;
        System.out.println(a / b + " " + a % b + " " + (-a / b) + " " + (-a % b) + " " + (a << 3) + " " + (-a >> 1) + " " + (-a >>> 28));
        long big = 1L << 40;
        System.out.println(big + " " + fact(20) + " " + Long.MAX_VALUE + " " + (Integer.MAX_VALUE + 1));
        double d = 10.0 / 4;
        System.out.println(d + " " + (1.0 / 3) + " " + 1e10 + " " + 0.0001 + " " + 100.0 + " " + (float) 0.1 + " " + (int) 3.99 + " " + (int) -3.99);
        System.out.println(Math.sqrt(2) + " " + Math.pow(2, 10) + " " + Math.abs(-7) + " " + Math.max(3, 9) + " " + Math.round(2.5) + " " + Math.round(-2.5) + " " + Math.floor(-1.5));
        System.out.println((char) ('a' + 2) + " " + (int) 'Z' + " " + Character.isDigit('7') + " " + Character.toUpperCase('q'));
        System.out.println(Integer.toBinaryString(42) + " " + Integer.toHexString(-1) + " " + Integer.parseInt("-123") + " " + Integer.valueOf(77) + " " + Double.parseDouble("2.5e3"));
        System.out.println(fib(15) + " " + squares[4] + " " + Arrays.toString(squares));
        System.out.println(0.1 + 0.2);
        System.out.println(5 / 2.0f);

        // Strings.
        String s = "Dolomiti, Tre Cime, Sassolungo";
        String[] parts = s.split(", ");
        System.out.println(parts.length + " " + parts[1].toUpperCase() + " " + s.indexOf("Cime") + " " + s.substring(10, 13) + " " + s.charAt(0) + s.length());
        System.out.println("  spazi  ".trim() + "|" + "a-b-c".replace('-', '+') + "|" + "abc".compareTo("abd") + "|" + "abc".lastIndexOf('c') + "|" +String.join("/", "a", "b"));
        System.out.println("uno due  tre".split("\\s+").length + " " + "a1b22c333".replaceAll("[0-9]+", "#") + " " + "2024-10-29".matches("\\d{4}-\\d{2}-\\d{2}"));
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < 5; i++) sb.append(i).append(',');
        sb.setLength(sb.length() - 1);
        sb.insert(0, "[").append(']');
        System.out.println(sb + " " + sb.reverse() + " " + "hash=" + "Dolomiti".hashCode());
        System.out.println(kind("pera") + " " + kind("carota") + " " + kind("sasso"));
        System.out.println(String.format("|%5d|%-5s|%05.1f|%x|%c|%b|%,d|%.3e|", 42, "ab", 3.14159, 255, 'z', true, 1234567, 12345.678));
        String n = null;
        System.out.println("null? " + n + " " + (n == null));

        // Objects.
        List<Base> shapes = new ArrayList<>();
        shapes.add(new Circle(1));
        shapes.add(new Rect(2, 3));
        shapes.add(new Rect(2, 2));
        Collections.sort(shapes);
        System.out.println(shapes + " made=" + Base.made);
        Shape first = shapes.get(0);
        System.out.println(first.name() + " " + (first instanceof Rect) + " " + (first instanceof Comparable));
        Collections.sort(shapes, new Comparator<Base>() {
            public int compare(Base x, Base y) {
                return x.name().compareTo(y.name());
            }
        });
        System.out.println(shapes);

        // Collections.
        Map<String, Integer> count = new HashMap<>();
        for (String w : "la neve la roccia la neve".split(" ")) {
            count.put(w, count.getOrDefault(w, 0) + 1);
        }
        TreeMap<String, Integer> sorted = new TreeMap<>(count);
        System.out.println(sorted + " " + sorted.firstKey() + " " + count.get("neve") + " " + count.containsKey("sole"));
        int total = 0;
        for (Map.Entry<String, Integer> e : sorted.entrySet()) total += e.getValue();
        System.out.println("totale " + total);
        Set<Point> pts = new HashSet<>();
        pts.add(new Point(1, 2));
        pts.add(new Point(1, 2));
        pts.add(new Point(3, 4));
        System.out.println(pts.size() + " " + pts.contains(new Point(3, 4)));
        List<Integer> nums = new ArrayList<>(Arrays.asList(5, 3, 9, 1));
        Iterator<Integer> it = nums.iterator();
        while (it.hasNext()) if (it.next() == 9) it.remove();
        Collections.sort(nums);
        System.out.println(nums + " max=" + Collections.max(nums) + " " + nums.indexOf(5));
        Deque<String> stack = new ArrayDeque<>();
        stack.push("a");
        stack.push("b");
        System.out.println(stack.pop() + stack.peek() + stack.size());
        TreeSet<String> ts = new TreeSet<>(Arrays.asList("pino", "abete", "larice"));
        System.out.println(ts + " " + ts.first());
        int[] arr = {5, 2, 8, 1};
        Arrays.sort(arr);
        int[][] grid = new int[2][3];
        grid[1][2] = 7;
        System.out.println(Arrays.toString(arr) + " " + Arrays.deepToString(grid) + " " + grid[1].length);

        // Enums.
        for (Day x : Day.values()) System.out.print(x + ":" + x.ordinal() + ":" + day(x) + " ");
        System.out.println(Day.valueOf("MAR").name());

        // Exceptions.
        System.out.println(divide(10, 2));
        System.out.println(divide(1, 0));
        try {
            Object o = "testo";
            Integer i = (Integer) o;
            System.out.println(i);
        } catch (ClassCastException e) {
            System.out.println("cast fallito");
        }
        try {
            int[] small = new int[2];
            small[5] = 1;
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("indice: " + e.getMessage());
        }
        try {
            Integer.parseInt("abc");
        } catch (NumberFormatException e) {
            System.out.println(e);
        }
        try {
            throw new IllegalStateException("stato");
        } catch (RuntimeException e) {
            System.out.println(e.getClass().getSimpleName() + " " + e.getMessage());
        }
        try {
            String z = null;
            z.length();
        } catch (NullPointerException e) {
            System.out.println("NPE presa");
        }

        // Random with a seed is the same everywhere.
        Random r = new Random(42);
        System.out.println(r.nextInt(100) + " " + r.nextInt(100) + " " + r.nextInt() + " " + r.nextBoolean() + " " + r.nextDouble());

        // Wrappers.
        Integer x1 = 1000, x2 = 1000;
        System.out.println(x1.equals(x2) + " " + (x1.intValue() == x2) + " " + Long.valueOf(5).hashCode() + " " + Double.valueOf(1.5));
        System.out.println("fine");
    }
}
