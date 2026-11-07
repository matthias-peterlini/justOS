import android.app.Activity;
import android.os.Bundle;
import android.os.Handler;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

/** An app that tests itself on justOS: it opens its window, presses its own
 *  button from a timer, logs what happened and closes. */
public class SelfTest extends Activity {
    private int clicks = 0;

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        setTitle("SelfTest");
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        final TextView label = new TextView(this);
        label.setText("0");
        root.addView(label);
        final Button button = new Button(this);
        button.setText("Premi");
        button.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                clicks++;
                label.setText("clic " + clicks);
            }
        });
        root.addView(button);
        setContentView(root);
        Log.i("selftest", "window open");
        final Handler h = new Handler();
        h.postDelayed(new Runnable() {
            @Override
            public void run() {
                button.performClick();
                button.performClick();
                Log.i("selftest", "label " + label.getText());
                finish();
            }
        }, 300);
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i("selftest", "destroyed after " + clicks + " clicks");
    }
}
