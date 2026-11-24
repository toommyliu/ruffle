package {
import flash.display.Sprite;
import flash.events.Event;
import flash.events.MouseEvent;
import flash.events.TimerEvent;
import flash.utils.Timer;

public class Test extends Sprite {
    private var frame:int = 0;
    private var slider:Sprite;
    private var popup:Sprite;

    public function Test() {
        slider = box("slider", 0, 130);
        addChild(slider);
        popup = box("popup", 130, 130);
        for each (var target:Sprite in [slider, popup]) {
            target.addEventListener(MouseEvent.ROLL_OVER, report);
            target.addEventListener(MouseEvent.ROLL_OUT, report);
        }
        addEventListener(Event.ENTER_FRAME, onFrame);
    }

    private function onFrame(e:Event):void {
        frame++;
        trace("frame " + frame);
        if (frame <= 5) {
            slider.x += 30;
        } else if (frame == 7) {
            slider.visible = false;
        } else if (frame == 9) {
            addChild(popup);
        } else if (frame == 11 || frame == 13) {
            removeChild(popup);
        } else if (frame == 12) {
            var timer:Timer = new Timer(1, 1);
            timer.addEventListener(TimerEvent.TIMER, function(e:TimerEvent):void {
                trace("timer shows popup");
                addChild(popup);
            });
            timer.start();
        }
    }

    private function report(e:MouseEvent):void {
        trace(e.type + " on " + e.currentTarget.name);
    }

    private function box(name:String, x:Number, y:Number):Sprite {
        var s:Sprite = new Sprite();
        s.name = name;
        s.graphics.beginFill(0xff0000);
        s.graphics.drawRect(0, 0, 40, 40);
        s.x = x;
        s.y = y;
        return s;
    }
}
}
