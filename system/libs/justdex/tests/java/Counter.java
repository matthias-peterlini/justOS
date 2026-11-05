package arcos.demo;

import android.app.Activity;
import android.graphics.Color;
import android.os.Bundle;
import android.os.Handler;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

/** An Android app built in code: a counter, a greeting and a timer. */
public class MainActivity extends Activity {
    private int count = 0;
    private TextView label;
    private EditText name;
    private final Handler handler = new Handler();
    private int ticks = 0;

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        setTitle("Contatore");
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(32, 32, 32, 32);

        TextView title = new TextView(this);
        title.setText("Ciao da Android!");
        title.setTextSize(24);
        title.setTextColor(Color.parseColor("#6200EE"));
        root.addView(title);

        label = new TextView(this);
        label.setId(1);
        label.setText("Premuto 0 volte");
        root.addView(label);

        LinearLayout row = new LinearLayout(this);
        Button plus = new Button(this);
        plus.setId(2);
        plus.setText("+1");
        plus.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count++;
                label.setText("Premuto " + count + (count == 1 ? " volta" : " volte"));
                if (count % 5 == 0) Toast.makeText(MainActivity.this, "Già " + count + "!", Toast.LENGTH_SHORT).show();
            }
        });
        row.addView(plus);
        Button reset = new Button(this);
        reset.setText("Azzera");
        reset.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count = 0;
                label.setText("Premuto 0 volte");
            }
        });
        row.addView(reset);
        root.addView(row);

        name = new EditText(this);
        name.setId(3);
        name.setHint("Il tuo nome");
        root.addView(name);
        Button hello = new Button(this);
        hello.setId(4);
        hello.setText("Saluta");
        hello.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String n = name.getText().toString().trim();
                ((TextView) findViewById(1)).setText(n.isEmpty() ? "Scrivi un nome" : "Ciao " + n + "!");
            }
        });
        root.addView(hello);

        CheckBox box = new CheckBox(this);
        box.setId(5);
        box.setText("Montagna");
        root.addView(box);

        setContentView(root);
        Log.d("Contatore", "creato");
        handler.postDelayed(new Runnable() {
            @Override
            public void run() {
                ticks++;
                setTitle("Contatore (" + ticks + ")");
            }
        }, 10);
    }
}
