package {
    import flash.display.Loader;
    import flash.display.LoaderInfo;
    import flash.display.Sprite;
    import flash.events.Event;
    import flash.events.UncaughtErrorEvent;
    import flash.external.ExternalInterface;
    import flash.net.URLRequest;

    public class Middle extends Sprite {
        private var offList:Loader = new Loader();
        private var onList:Loader = new Loader();
        private var loaded:int = 0;
        private var frames:int = 0;
        private var parts:Array = [];

        public function Middle() {
            listen(loaderInfo, "middle loaderInfo");
            load(offList, "off-list");
            load(onList, "on-list");
            addChild(onList);
        }

        private function load(loader:Loader, name:String):void {
            listen(loader.contentLoaderInfo, name + " child loaderInfo");
            loader.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, listener(name + " Loader"));
            loader.contentLoaderInfo.addEventListener(Event.COMPLETE, onComplete);
            loader.load(new URLRequest("child.swf"));
        }

        private function listen(info:LoaderInfo, name:String):void {
            info.uncaughtErrorEvents.addEventListener(UncaughtErrorEvent.UNCAUGHT_ERROR, listener(name));
        }

        private function listener(name:String):Function {
            return function(e:UncaughtErrorEvent):void {
                log(name + " (phase " + e.eventPhase + ")");
            };
        }

        private function onComplete(e:Event):void {
            loaded++;
            if (loaded == 2) {
                addEventListener(Event.ENTER_FRAME, onEnterFrame);
            }
        }

        private function throwFrom(loader:Loader, createdByChild:Boolean):void {
            var domain:* = loader.contentLoaderInfo.applicationDomain;
            var partClass:Object = domain.getDefinition("Part");
            partClass.throwNext = true;
            parts.push(createdByChild ? domain.getDefinition("PartFactory").make() : new (partClass as Class)());
        }

        private function onEnterFrame(e:Event):void {
            frames++;
            switch (frames) {
                case 1:
                    log("clip created by the middle SWF");
                    throwFrom(offList, false);
                    break;
                case 6:
                    log("clip created by a child whose Loader isn't on the display list");
                    throwFrom(offList, true);
                    break;
                case 11:
                    log("clip created by a child whose Loader is in the middle SWF");
                    throwFrom(onList, true);
                    break;
                case 16:
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
