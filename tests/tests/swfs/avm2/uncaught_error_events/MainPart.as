package {
    import flash.display.MovieClip;

    public class MainPart extends MovieClip {
        public static var ran:int = 0;
        public static var throwOn:int = 0;

        public function MainPart() {
            addFrameScript(0, frame1, 1, frame2);
        }

        private function frame1():void {
            ran++;
            stop();
            if (throwOn & 1) {
                throw new Error("main frame 1 script");
            }
        }

        private function frame2():void {
            ran++;
            if (throwOn & 2) {
                throw new Error("main frame 2 script");
            }
        }
    }
}
