package {
    import flash.display.Loader;
    import flash.display.Sprite;
    import flash.events.UncaughtErrorEvent;
    import flash.external.ExternalInterface;
    import flash.net.URLRequest;

    public class Test extends Sprite {
        private var loader:Loader = new Loader();

        public function Test() {
            loaderInfo.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, listener("root loaderInfo"));
            loader.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, listener("middle Loader"));
            loader.load(new URLRequest("middle.swf"));
            addChild(loader);
        }

        private function listener(name:String):Function {
            return function(e:UncaughtErrorEvent):void {
                var line:String = name + " (phase " + e.eventPhase + ")";
                trace(line);
                if (ExternalInterface.available) {
                    ExternalInterface.call("report", line);
                }
            };
        }
    }
}
