package {
    import flash.display.MovieClip;
    import flash.events.Event;
    import flash.events.UncaughtErrorEvent;
    import flash.external.ExternalInterface;
    import flash.utils.getDefinitionByName;

    public class Test extends MovieClip {
        private var parts:Array = [];
        private var errors:Array = [];
        private var frames:int = 0;

        public function Test() {
            loaderInfo.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, onUncaughtError);
            make("offStage", false, 2);
            make("onStage", true, 2);
            make("playedLater", true, 2);
            make("gotoAndPlayThenThrow", true, 2).gotoFirst = 3;
            make("playedByHandler", true, 2);
            var listened:* = make("enterFrameListenerThrows", true, 0);
            var thrown:Boolean = false;
            listened.addEventListener(Event.ENTER_FRAME, function(e:Event):void {
                if (!thrown) {
                    thrown = true;
                    throw new Error("enterFrameListenerThrows");
                }
            });
            addEventListener(Event.ENTER_FRAME, onEnterFrame);
        }

        private function make(name:String, onStage:Boolean, throwAt:int):* {
            var part:* = new (getDefinitionByName("Part") as Class)();
            part.name = name;
            part.throwAt = throwAt;
            if (onStage) {
                addChild(part);
            }
            parts.push(part);
            return part;
        }

        private function onUncaughtError(e:UncaughtErrorEvent):void {
            var name:String = (e.error as Error).message;
            errors.push(name);
            if (name == "playedByHandler") {
                parts[4].play();
            }
        }

        private function onEnterFrame(e:Event):void {
            frames++;
            var at:Array = [];
            for each (var part:* in parts) {
                at.push(part.name + " " + part.currentFrame);
            }
            log("frame " + frames + ": " + at.join(", ") + (errors.length == 0 ? "" : "; threw: " + errors.sort().join(", ")));
            errors = [];
            if (frames == 4) {
                parts[2].play();
            }
            if (frames == 7) {
                removeEventListener(Event.ENTER_FRAME, onEnterFrame);
                if (ExternalInterface.available) {
                    ExternalInterface.call("report", "done");
                }
            }
        }

        private function log(line:String):void {
            trace(line);
            if (ExternalInterface.available) {
                ExternalInterface.call("report", line);
            }
        }
    }
}
