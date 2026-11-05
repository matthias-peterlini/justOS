import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.util.Scanner;

/** Reads from the keyboard: the input comes from the test. */
public class Input {
    public static void main(String[] args) throws Exception {
        Scanner in = new Scanner(System.in);
        System.out.print("Come ti chiami? ");
        String name = in.nextLine();
        System.out.print("Quanti numeri? ");
        int n = in.nextInt();
        int sum = 0;
        for (int i = 0; i < n; i++) sum += in.nextInt();
        double d = in.nextDouble();
        in.nextLine();
        System.out.println("Ciao " + name + ", somma " + sum + ", media " + (double) sum / n + ", e " + d * 2);
        String word = in.next();
        System.out.println("parola: " + word + " " + in.hasNextInt());
        while (in.hasNextLine()) {
            String line = in.nextLine();
            if (line.isEmpty()) continue;
            System.out.println("riga: " + line);
        }
        System.exit(3);
    }
}
