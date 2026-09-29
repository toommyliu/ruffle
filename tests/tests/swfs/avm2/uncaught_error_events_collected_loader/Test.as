package {
    import flash.display.Loader;
    import flash.display.Sprite;
    import flash.events.Event;
    import flash.events.UncaughtErrorEvent;
    import flash.external.ExternalInterface;
    import flash.net.URLRequest;

    public class Test extends Sprite {
        private var loader:Loader = new Loader();
        private var partClass:Object;
        private var parts:Array = [];
        private var frames:int = 0;
        private var garbage:Array;

        public function Test() {
            loaderInfo.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, onUncaughtError);
            loader.contentLoaderInfo.addEventListener(Event.COMPLETE, onComplete);
            loader.load(new URLRequest("child.swf"));
        }

        private function onComplete(e:Event):void {
            partClass = loader.contentLoaderInfo.applicationDomain.getDefinition("Part");
            for (var i:int = 0; i < 3; i++) {
                parts.push(new partClass());
            }
            loader = null;
            addEventListener(Event.ENTER_FRAME, onEnterFrame);
        }

        private function onEnterFrame(e:Event):void {
            // Allocates so that the Loader is collected before the parts throw.
            garbage = [];
            for (var i:int = 0; i < 20000; i++) {
                garbage.push({i: i});
            }
            frames++;
            if (frames == 40) {
                partClass.throwing = true;
            }
        }

        private function onUncaughtError(e:UncaughtErrorEvent):void {
            var message:String = (e.error as Error).message;
            log("main loaderInfo: " + message);
            if (message == "child frame script 3") {
                partClass.throwing = false;
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
